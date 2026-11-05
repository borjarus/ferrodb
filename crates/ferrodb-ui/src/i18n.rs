//! Translations (Fluent). Polish and English from the start (plan, section 3).
//!
//! Messages live in `assets/i18n/<locale>/main.ftl` and are embedded at build
//! time. Look-ups never fail: a missing message falls back to English and then
//! to the message id itself (and is logged once in debug builds).

use std::sync::{OnceLock, RwLock};

use fluent_bundle::{FluentArgs, FluentResource, FluentValue, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Locale {
    #[default]
    En,
    Pl,
}

impl Locale {
    pub const ALL: [Locale; 2] = [Locale::En, Locale::Pl];

    pub fn id(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Pl => "pl",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|l| l.id() == id || id.starts_with(&format!("{}-", l.id())))
    }

    /// Endonym shown in the language picker.
    pub fn native_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::Pl => "Polski",
        }
    }

    fn source(self) -> &'static str {
        match self {
            Self::En => include_str!("../../../assets/i18n/en/main.ftl"),
            Self::Pl => include_str!("../../../assets/i18n/pl/main.ftl"),
        }
    }

    /// Best guess from the OS (`LANG`, `LC_ALL`); defaults to English.
    pub fn from_system() -> Self {
        ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find_map(|v| Self::from_id(&v.to_ascii_lowercase()))
            .unwrap_or_default()
    }
}

struct Bundles {
    en: FluentBundle<FluentResource>,
    pl: FluentBundle<FluentResource>,
}

fn bundle(locale: Locale) -> FluentBundle<FluentResource> {
    let lang: LanguageIdentifier = locale.id().parse().expect("valid language id");
    let mut b = FluentBundle::new_concurrent(vec![lang]);
    b.set_use_isolating(false);
    let res = FluentResource::try_new(locale.source().to_owned())
        .unwrap_or_else(|(_, errs)| panic!("invalid FTL for {}: {errs:?}", locale.id()));
    b.add_resource(res).expect("no duplicate messages");
    b
}

fn bundles() -> &'static Bundles {
    static B: OnceLock<Bundles> = OnceLock::new();
    B.get_or_init(|| Bundles {
        en: bundle(Locale::En),
        pl: bundle(Locale::Pl),
    })
}

static CURRENT: RwLock<Locale> = RwLock::new(Locale::En);

pub fn set_locale(locale: Locale) {
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) = locale;
}

pub fn locale() -> Locale {
    *CURRENT.read().unwrap_or_else(|e| e.into_inner())
}

fn format(
    b: &FluentBundle<FluentResource>,
    id: &str,
    args: Option<&FluentArgs<'_>>,
) -> Option<String> {
    let (id, attr) = id.split_once('.').map_or((id, None), |(m, a)| (m, Some(a)));
    let msg = b.get_message(id)?;
    let pattern = match attr {
        Some(a) => msg.get_attribute(a)?.value(),
        None => msg.value()?,
    };
    let mut errors = vec![];
    Some(b.format_pattern(pattern, args, &mut errors).into_owned())
}

fn lookup_in(locale: Locale, id: &str, args: Option<&FluentArgs<'_>>) -> String {
    let b = bundles();
    let primary = match locale {
        Locale::En => &b.en,
        Locale::Pl => &b.pl,
    };
    format(primary, id, args)
        .or_else(|| format(&b.en, id, args))
        .unwrap_or_else(|| {
            #[cfg(debug_assertions)]
            tracing::warn!(message = id, "missing translation");
            id.to_owned()
        })
}

/// Translates message `id` (`message` or `message.attribute`).
pub fn t(id: &str) -> String {
    lookup_in(locale(), id, None)
}

/// Translates with arguments: `t_args("rows-count", &[("count", 5.into())])`.
pub fn t_args(id: &str, args: &[(&str, FluentValue<'_>)]) -> String {
    let mut fa = FluentArgs::new();
    for (k, v) in args {
        fa.set(*k, v.clone());
    }
    lookup_in(locale(), id, Some(&fa))
}

/// Message ids defined in a locale (for completeness tests).
pub fn message_ids(locale: Locale) -> Vec<String> {
    let res = FluentResource::try_new(locale.source().to_owned()).expect("valid FTL");
    res.entries()
        .filter_map(|e| match e {
            fluent_syntax::ast::Entry::Message(m) => Some(m.id.name.to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polish_and_english_define_the_same_messages() {
        let mut en = message_ids(Locale::En);
        let mut pl = message_ids(Locale::Pl);
        en.sort();
        pl.sort();
        let missing_pl: Vec<_> = en.iter().filter(|m| !pl.contains(m)).collect();
        let extra_pl: Vec<_> = pl.iter().filter(|m| !en.contains(m)).collect();
        assert!(missing_pl.is_empty(), "missing in pl: {missing_pl:?}");
        assert!(extra_pl.is_empty(), "only in pl: {extra_pl:?}");
    }

    #[test]
    fn lookup_falls_back_to_id() {
        assert_eq!(
            lookup_in(Locale::Pl, "no-such-message", None),
            "no-such-message"
        );
        assert_eq!(lookup_in(Locale::Pl, "app-name", None), "FerroDb");
    }
}
