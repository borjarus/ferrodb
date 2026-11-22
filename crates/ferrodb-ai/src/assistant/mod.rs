//! Natural-language query assistant (plan, phase 9.1).
//!
//! [`Assistant::ask`](AssistantService::ask) builds a schema-aware prompt for
//! the request's data source ([`context`]), streams the answer from the
//! configured provider — Anthropic Messages API, OpenAI-compatible Chat
//! Completions or Ollama ([`providers`]) — and checks the code blocks of the
//! answer with the language service ([`answer`]). Nothing is ever run.
//!
//! Events of one `ask`: `Context` first, then `Delta`s, then exactly one of
//! `Finished` / `Failed`. Cancelling the handle stops reading the response
//! (the connection is dropped, generation stops server-side) and ends with
//! `Failed("Cancelled.")`; so does dropping the event receiver. An answer
//! cut off at `max_tokens` finishes normally with a note appended (also
//! streamed as a last `Delta`). A refusal fails with an explanation; the
//! text already streamed stays with the UI.
//!
//! Settings live in `<config_dir>/ai.toml`, the API key of each provider in
//! the secret store under `secret://ai-<provider>` (key `api_key`). Keyring
//! and file access always run on the blocking pool of the shared runtime.

mod answer;
mod context;
mod http;
mod prompt;
mod providers;
mod settings;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use ferrodb_api::{
    AiProvider, AiSettings, AssistantAnswer, AssistantEvent, AssistantHandle, AssistantRequest,
    AssistantService, Backend, BoxFuture, CancelHandle, Error, Result,
};

use crate::{AiOptions, runtime::runtime};

/// An error shown to the user as is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AiError(pub String);

impl std::fmt::Display for AiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<AiError> for Error {
    fn from(e: AiError) -> Self {
        Error::Other(e.0)
    }
}

const CANCELLED: &str = "Cancelled.";

pub struct Assistant {
    inner: Arc<Inner>,
}

struct Inner {
    backend: Arc<dyn Backend>,
    options: AiOptions,
    state: Mutex<State>,
    http: OnceLock<std::result::Result<reqwest::Client, AiError>>,
    /// `true` once `has_key` of the loaded settings is known.
    key_loaded: tokio::sync::watch::Sender<bool>,
}

struct State {
    settings: AiSettings,
    /// Bumped by every save: a key check started before it is stale.
    generation: u64,
}

/// Runs `f` on the blocking pool of the shared runtime (keyring, files).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> std::result::Result<T, AiError> + Send + 'static,
) -> std::result::Result<T, AiError> {
    runtime()
        .spawn_blocking(f)
        .await
        .map_err(|e| AiError(format!("internal error: {e}")))?
}

/// Runs `fut` on the shared runtime (reqwest needs tokio) and awaits it from
/// any executor.
async fn on_runtime<T: Send + 'static>(
    fut: impl Future<Output = std::result::Result<T, AiError>> + Send + 'static,
) -> std::result::Result<T, AiError> {
    runtime()
        .spawn(fut)
        .await
        .map_err(|e| AiError(format!("internal error: {e}")))?
}

impl Assistant {
    /// Loads `ai.toml`; whether a key is stored is checked in the background
    /// (see [`Self::loaded`]).
    pub fn new(backend: Arc<dyn Backend>, options: AiOptions) -> Arc<Self> {
        let settings = settings::load(options.config_dir.as_deref());
        let (key_loaded, _) = tokio::sync::watch::channel(false);
        let inner = Arc::new(Inner {
            backend,
            options,
            state: Mutex::new(State {
                settings,
                generation: 0,
            }),
            http: OnceLock::new(),
            key_loaded,
        });
        let weak: Weak<Inner> = Arc::downgrade(&inner);
        runtime().spawn(async move {
            if let Some(inner) = weak.upgrade() {
                inner.refresh_has_key().await;
                inner.key_loaded.send_replace(true);
            }
        });
        Arc::new(Self { inner })
    }

    /// Resolves once `settings().has_key` reflects the secret store.
    pub async fn loaded(&self) {
        let mut rx = self.inner.key_loaded.subscribe();
        let _ = rx.wait_for(|v| *v).await;
    }
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn refresh_has_key(&self) {
        let (provider, generation) = {
            let s = self.state();
            (s.settings.provider, s.generation)
        };
        let store = self.options.secrets.clone();
        let has_key = blocking(move || settings::read_key(&*store, provider))
            .await
            .map(|k| k.is_some())
            .unwrap_or_else(|e| {
                tracing::warn!("AI assistant: {e}");
                false
            });
        let mut s = self.state();
        if s.generation == generation {
            s.settings.has_key = has_key;
        }
    }

    fn client(&self) -> std::result::Result<reqwest::Client, AiError> {
        self.http.get_or_init(http::client).clone()
    }

    async fn key(&self, provider: AiProvider) -> std::result::Result<Option<String>, AiError> {
        let store = self.options.secrets.clone();
        blocking(move || settings::read_key(&*store, provider)).await
    }

    async fn run(
        &self,
        req: AssistantRequest,
        settings: AiSettings,
        model: String,
        tx: &async_channel::Sender<AssistantEvent>,
        cancel: &CancelHandle,
    ) -> std::result::Result<AssistantAnswer, AiError> {
        let cancelled = || cancel.is_cancelled() || tx.is_closed();
        let key = self.key(settings.provider).await?;
        if settings.provider.needs_key() && key.is_none() {
            return Err(AiError(format!(
                "No API key is stored for {}: add one in Settings → AI assistant.",
                settings.provider.label()
            )));
        }
        let ctx = context::build(&*self.backend, &req, &settings, &cancelled).await;
        if cancelled() {
            return Err(AiError(CANCELLED.into()));
        }
        let chat = prompt::chat(&req, &ctx, model.clone(), settings.max_tokens);
        let _ = tx
            .send(AssistantEvent::Context {
                objects: ctx.objects.clone(),
                chars: chat.chars(),
            })
            .await;
        let client = self.client()?;
        let target = providers::Target {
            provider: settings.provider,
            endpoint: settings.effective_endpoint(),
            key: key.as_deref(),
        };
        let mut on_delta = |d: &str| {
            let _ = tx.try_send(AssistantEvent::Delta(d.to_owned()));
        };
        let outcome = providers::stream(&client, target, &chat, &cancelled, &mut on_delta).await?;
        let mut text = outcome.text;
        match outcome.stop {
            providers::Stop::Cancelled => return Err(AiError(CANCELLED.into())),
            providers::Stop::Refusal(message) => return Err(AiError(message)),
            providers::Stop::MaxTokens => {
                let note = format!(
                    "\n\n*(The answer was cut off at the limit of {} tokens; raise \"Max tokens\" in Settings → AI assistant.)*",
                    settings.max_tokens
                );
                on_delta(&note);
                text.push_str(&note);
            }
            providers::Stop::End => {}
        }
        let queries = answer::queries(&*self.backend, &text, &ctx).await;
        Ok(AssistantAnswer {
            text,
            queries,
            usage: outcome.usage,
            model: outcome.model.unwrap_or(model),
        })
    }
}

impl AssistantService for Assistant {
    fn settings(&self) -> AiSettings {
        self.inner.state().settings.clone()
    }

    fn save_settings(
        &self,
        settings: AiSettings,
        api_key: Option<String>,
    ) -> BoxFuture<'_, Result<()>> {
        let inner = self.inner.clone();
        Box::pin(async move {
            let dir = inner.options.config_dir.clone();
            let store = inner.options.secrets.clone();
            let provider = settings.provider;
            let to_save = settings.clone();
            let has_key = blocking(move || {
                if let Some(dir) = &dir {
                    settings::save(dir, &to_save)?;
                }
                if let Some(key) = api_key {
                    settings::write_key(&*store, provider, key.trim())?;
                }
                Ok(settings::read_key(&*store, provider)?.is_some())
            })
            .await?;
            let mut s = inner.state();
            s.settings = AiSettings {
                has_key,
                ..settings
            };
            s.generation += 1;
            Ok(())
        })
    }

    fn test(&self) -> BoxFuture<'_, Result<String>> {
        let inner = self.inner.clone();
        let settings = self.settings();
        Box::pin(async move {
            let Some(model) = settings.effective_model().map(str::to_owned) else {
                return Err(Error::Other(format!(
                    "Choose a model for {}.",
                    settings.provider.label()
                )));
            };
            Ok(on_runtime(async move {
                let key = inner.key(settings.provider).await?;
                if settings.provider.needs_key() && key.is_none() {
                    return Err(AiError(format!(
                        "No API key is stored for {}.",
                        settings.provider.label()
                    )));
                }
                let target = providers::Target {
                    provider: settings.provider,
                    endpoint: settings.effective_endpoint(),
                    key: key.as_deref(),
                };
                providers::test(&inner.client()?, target, &model).await
            })
            .await?)
        })
    }

    fn models(&self) -> BoxFuture<'_, Result<Vec<String>>> {
        let inner = self.inner.clone();
        let settings = self.settings();
        Box::pin(async move {
            Ok(on_runtime(async move {
                let key = inner.key(settings.provider).await?;
                if settings.provider.needs_key() && key.is_none() {
                    return Err(AiError(format!(
                        "No API key is stored for {}.",
                        settings.provider.label()
                    )));
                }
                let target = providers::Target {
                    provider: settings.provider,
                    endpoint: settings.effective_endpoint(),
                    key: key.as_deref(),
                };
                providers::models(&inner.client()?, target).await
            })
            .await?)
        })
    }

    fn ask(&self, request: AssistantRequest) -> AssistantHandle {
        let settings = self.settings();
        if !settings.enabled {
            return AssistantHandle::failed(
                "The AI assistant is turned off: enable it in Settings → AI assistant.",
            );
        }
        let Some(model) = settings.effective_model().map(str::to_owned) else {
            return AssistantHandle::failed(format!(
                "No model is chosen for {}: pick one in Settings → AI assistant.",
                settings.provider.label()
            ));
        };
        if request.prompt.trim().is_empty()
            && request
                .context
                .as_deref()
                .is_none_or(|c| c.trim().is_empty())
        {
            return AssistantHandle::failed("Type a question first.");
        }
        let (tx, rx) = async_channel::unbounded();
        let cancel = CancelHandle::new();
        let inner = self.inner.clone();
        let task_cancel = cancel.clone();
        runtime().spawn(async move {
            let event = match inner.run(request, settings, model, &tx, &task_cancel).await {
                Ok(answer) => AssistantEvent::Finished(answer),
                Err(e) => AssistantEvent::Failed(e.0),
            };
            let _ = tx.send(event).await;
        });
        AssistantHandle { events: rx, cancel }
    }
}
