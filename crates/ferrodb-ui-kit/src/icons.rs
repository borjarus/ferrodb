//! Icons used by FerroDb, mapped from domain concepts to Lucide icons.

use ferrodb_api::{DataModel, ObjectKind};
pub use gpui_kit::assets::IconName as Lucide;
pub use gpui_kit::component::{Icon, IconName};

/// Icon for an explorer object kind.
pub fn object_icon(kind: ObjectKind) -> Icon {
    use ObjectKind as K;
    let name = match kind {
        K::Database | K::Project => Lucide::Database,
        K::Schema | K::Keyspace | K::Dataset | K::Scope => Lucide::Layers,
        K::Bucket => Lucide::Archive,
        K::Folder => Lucide::Folder,
        K::Table => Lucide::Table,
        K::View | K::MaterializedView => Lucide::Eye,
        K::Column | K::Field => Lucide::Columns2,
        K::Index => Lucide::ListOrdered,
        K::PrimaryKey => Lucide::KeyRound,
        K::ForeignKey => Lucide::Link,
        K::Procedure | K::Function => Lucide::SquareFunction,
        K::Sequence => Lucide::ListOrdered,
        K::Trigger => Lucide::Zap,
        K::Collection => Lucide::Braces,
        K::KeyPrefix => Lucide::FolderTree,
        K::Key => Lucide::Key,
    };
    Icon::from(name)
}

/// Icon for a data source by its model.
pub fn data_source_icon(model: DataModel) -> Icon {
    Icon::from(match model {
        DataModel::Tabular => Lucide::Database,
        DataModel::Document => Lucide::FileBraces,
        DataModel::KeyValue => Lucide::KeyRound,
    })
}

pub fn console_icon() -> Icon {
    Icon::from(Lucide::SquareTerminal)
}
