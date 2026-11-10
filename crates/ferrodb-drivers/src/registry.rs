//! Drivers compiled into this build, plus helpers shared by descriptors.

use std::sync::Arc;

use ferrodb_api::{
    AccessLayer, AuthMethod, Capabilities, DataModel, DriverCategory, DriverDescriptor, DriverId,
    DriverProperty, ExplainSupport, HierarchyLevel, HierarchySpec, LanguageLevel, ObjectKind,
    QueryLanguage, SchemaKind, TxSupport,
};

use crate::Driver;

#[derive(Clone, Default)]
pub struct Registry {
    drivers: Vec<Arc<dyn Driver>>,
}

impl Registry {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Every native driver enabled by feature flags.
    pub fn builtin() -> Self {
        #[allow(unused_mut)]
        let mut r = Self::empty();
        #[cfg(feature = "pgwire")]
        r.drivers.extend(crate::pgwire::drivers());
        #[cfg(feature = "mysql")]
        r.drivers.extend(crate::mysql::drivers());
        #[cfg(feature = "sqlite")]
        r.drivers.extend(crate::sqlite::drivers());
        #[cfg(feature = "mongodb")]
        r.drivers.extend(crate::mongodb::drivers());
        r
    }

    pub fn register(&mut self, driver: Arc<dyn Driver>) {
        self.drivers.push(driver);
    }

    pub fn get(&self, id: &DriverId) -> Option<Arc<dyn Driver>> {
        self.drivers.iter().find(|d| &d.id() == id).cloned()
    }

    pub fn list(&self) -> &[Arc<dyn Driver>] {
        &self.drivers
    }
}

/// Capabilities of a relational database with a native driver.
pub fn relational_capabilities(dialect: &str, auth: Vec<AuthMethod>) -> Capabilities {
    Capabilities {
        data_model: DataModel::Tabular,
        language: QueryLanguage::sql(dialect),
        access: AccessLayer::Native,
        language_level: LanguageLevel::A,
        transactions: TxSupport::MultiStatement,
        schema: SchemaKind::Strict,
        joins: true,
        // EXPLAIN as a diagram arrives in phase 7.
        explain: ExplainSupport::None,
        server_cancel: true,
        // Editing results arrives in phase 6.
        editable_results: false,
        cost_estimate: false,
        auth_methods: auth,
    }
}

pub fn property(key: &str, label: &str, default: Option<&str>, required: bool) -> DriverProperty {
    DriverProperty {
        key: key.into(),
        label: label.into(),
        default: default.map(str::to_owned),
        required,
        secret: false,
        auth: Vec::new(),
    }
}

pub fn secret_property(key: &str, label: &str, auth: &[AuthMethod]) -> DriverProperty {
    DriverProperty {
        secret: true,
        auth: auth.to_vec(),
        ..property(key, label, None, false)
    }
}

/// Host / port / database / user / password form.
pub fn network_properties(
    default_port: u16,
    default_database: Option<&str>,
) -> Vec<DriverProperty> {
    let port = default_port.to_string();
    vec![
        property("host", "Host", Some("localhost"), true),
        property("port", "Port", Some(&port), false),
        property("database", "Database", default_database, false),
        DriverProperty {
            auth: vec![AuthMethod::Password],
            ..property("user", "User", None, false)
        },
        secret_property("password", "Password", &[AuthMethod::Password]),
    ]
}

pub fn descriptor(
    id: &str,
    family: &str,
    name: &str,
    capabilities: Capabilities,
    properties: Vec<DriverProperty>,
    default_port: Option<u16>,
    url_template: &str,
) -> DriverDescriptor {
    DriverDescriptor {
        id: id.into(),
        family: family.into(),
        name: name.into(),
        category: DriverCategory::Relational,
        capabilities,
        requirements: Vec::new(),
        properties,
        default_port,
        url_template: Some(url_template.into()),
    }
}

/// `container → {tables, views} → columns`, the phase-3 explorer tree.
pub fn relational_hierarchy(container: ObjectKind) -> HierarchySpec {
    let level = |kind, children: &[ObjectKind]| HierarchyLevel {
        kind,
        children: children.to_vec(),
    };
    HierarchySpec {
        levels: vec![
            level(container, &[ObjectKind::Table, ObjectKind::View]),
            level(ObjectKind::Table, &[ObjectKind::Column]),
            level(ObjectKind::View, &[ObjectKind::Column]),
        ],
        key_separator: None,
    }
}
