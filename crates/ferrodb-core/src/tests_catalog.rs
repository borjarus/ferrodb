//! The real driver registry against the UI's mock catalog of the 26
//! databases (plan phase 8): every database the UI was designed with is
//! registered, with the same family, data model, language, access layer,
//! language level and authentication methods.

use ferrodb_api::Backend;

use crate::{CoreBackend, CoreOptions};

#[test]
fn every_database_of_the_catalog_is_registered() {
    let core = CoreBackend::new(CoreOptions::in_memory()).unwrap();
    let mut problems = Vec::new();
    let mock = ferrodb_mock::driver_catalog();
    assert_eq!(mock.len(), 26);
    for m in &mock {
        let Some(real) = core.drivers().get(&m.id) else {
            problems.push(format!("{}: not registered", m.id));
            continue;
        };
        let (mc, rc) = (&m.capabilities, &real.capabilities);
        let mut check = |what: &str, same: bool, detail: String| {
            if !same {
                problems.push(format!("{}: {what} differs ({detail})", m.id));
            }
        };
        check(
            "family",
            m.family == real.family,
            format!("{} / {}", m.family, real.family),
        );
        check(
            "data model",
            mc.data_model == rc.data_model,
            format!("{:?} / {:?}", mc.data_model, rc.data_model),
        );
        check(
            "language",
            mc.language == rc.language,
            format!("{:?} / {:?}", mc.language, rc.language),
        );
        check(
            "access",
            mc.access == rc.access,
            format!("{:?} / {:?}", mc.access, rc.access),
        );
        check(
            "language level",
            mc.language_level == rc.language_level,
            format!("{:?} / {:?}", mc.language_level, rc.language_level),
        );
        let missing: Vec<_> = mc
            .auth_methods
            .iter()
            .filter(|a| !rc.auth_methods.contains(a))
            .collect();
        check(
            "auth methods",
            missing.is_empty(),
            format!("missing {missing:?}"),
        );
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}
