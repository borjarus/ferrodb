//! Natural-language query assistant. (Phase 9.1 — stub, to be implemented.)

use std::sync::{Arc, Mutex};

use ferrodb_api::{
    AiSettings, AssistantHandle, AssistantRequest, AssistantService, Backend, BoxFuture, Error,
    Result,
};

use crate::AiOptions;

pub struct Assistant {
    #[allow(dead_code)]
    backend: Arc<dyn Backend>,
    #[allow(dead_code)]
    options: AiOptions,
    settings: Mutex<AiSettings>,
}

impl Assistant {
    pub fn new(backend: Arc<dyn Backend>, options: AiOptions) -> Arc<Self> {
        Arc::new(Self {
            backend,
            options,
            settings: Mutex::new(AiSettings::default()),
        })
    }
}

impl AssistantService for Assistant {
    fn settings(&self) -> AiSettings {
        self.settings.lock().unwrap().clone()
    }
    fn save_settings(
        &self,
        settings: AiSettings,
        _api_key: Option<String>,
    ) -> BoxFuture<'_, Result<()>> {
        *self.settings.lock().unwrap() = settings;
        Box::pin(async { Ok(()) })
    }
    fn test(&self) -> BoxFuture<'_, Result<String>> {
        Box::pin(async { Err(Error::Unsupported("not implemented yet".into())) })
    }
    fn ask(&self, _request: AssistantRequest) -> AssistantHandle {
        AssistantHandle::failed("not implemented yet")
    }
}
