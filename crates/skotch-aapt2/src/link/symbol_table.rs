//! Symbol resolution across included APKs and the table being linked.
//!
//! Port of `process/SymbolTable.{h,cpp}` (ResourceTableSymbolSource +
//! AssetManagerSymbolSource collapsed into one in-memory index).
//!
//! STUB: signatures are final; the implementation is being ported.

use crate::apk::LoadedApk;
use crate::res::table::{ResourceTable, VisibilityLevel};
use crate::res::value::Attribute;
use crate::res::{ResourceId, ResourceName};

/// A resolved symbol. Mirrors `SymbolTable::Symbol`.
#[derive(Debug, Clone, Default)]
pub struct Symbol {
    pub id: Option<ResourceId>,
    /// Set when the symbol is an attribute: its definition.
    pub attribute: Option<Attribute>,
    pub is_public: bool,
    pub is_dynamic: bool,
}

#[derive(Default)]
pub struct SymbolTable {
    includes: Vec<LoadedApk>,
    self_index: std::collections::HashMap<ResourceName, Symbol>,
    self_package: String,
}

impl SymbolTable {
    pub fn new() -> Self {
        SymbolTable::default()
    }

    /// Adds an `-I` include (framework APK / android.jar).
    pub fn add_include(&mut self, apk: LoadedApk) {
        self.includes.push(apk);
    }

    /// Indexes the table being linked so its own symbols resolve.
    /// Called after ID assignment.
    pub fn rebuild_self_index(
        &mut self,
        table: &ResourceTable,
        _package_id: u8,
        package_name: &str,
    ) {
        self.self_package = package_name.to_string();
        self.self_index.clear();
        for package in &table.packages {
            for ty in &package.types {
                for entry in &ty.entries {
                    let name = ResourceName::with_named_type(
                        if package.name.is_empty() {
                            package_name
                        } else {
                            &package.name
                        },
                        ty.named_type.clone(),
                        &entry.name,
                    );
                    let attribute = entry.values.iter().find_map(|cv| {
                        cv.value.as_ref().and_then(|v| match &v.kind {
                            crate::res::value::ValueKind::Attribute(attr) => Some(attr.clone()),
                            _ => None,
                        })
                    });
                    self.self_index.insert(
                        name,
                        Symbol {
                            id: entry.id,
                            attribute,
                            is_public: entry.visibility.level == VisibilityLevel::Public,
                            is_dynamic: false,
                        },
                    );
                }
            }
        }
    }

    /// Finds a symbol by name; `name.package` must already be
    /// fully-qualified (empty = the compilation package).
    pub fn find_by_name(&self, name: &ResourceName) -> Option<Symbol> {
        let lookup = if name.package.is_empty() {
            ResourceName::with_named_type(&self.self_package, name.ty.clone(), &name.entry)
        } else {
            name.clone()
        };
        if let Some(symbol) = self.self_index.get(&lookup) {
            return Some(symbol.clone());
        }
        // Fall back to the included APK tables (framework etc.).
        for include in &self.includes {
            if let Some(result) = include.table.find_resource(&lookup) {
                let attribute = result.entry.values.iter().find_map(|cv| {
                    cv.value.as_ref().and_then(|v| match &v.kind {
                        crate::res::value::ValueKind::Attribute(attr) => Some(attr.clone()),
                        _ => None,
                    })
                });
                return Some(Symbol {
                    id: result.entry.id,
                    attribute,
                    is_public: result.entry.visibility.level == VisibilityLevel::Public,
                    is_dynamic: false,
                });
            }
        }
        None
    }

    /// Finds a symbol by resource ID (used by dump/debug paths).
    pub fn find_by_id(&self, id: ResourceId) -> Option<Symbol> {
        for include in &self.includes {
            for package in &include.table.packages {
                for ty in &package.types {
                    for entry in &ty.entries {
                        if entry.id == Some(id) {
                            return Some(Symbol {
                                id: Some(id),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
        None
    }

    pub fn includes(&self) -> &[LoadedApk] {
        &self.includes
    }
}
