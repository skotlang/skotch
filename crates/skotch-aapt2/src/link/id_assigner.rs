//! Resource ID assignment.
//!
//! Port of `compile/IdAssigner.{h,cpp}`: assigns `0xPPTTEEEE` IDs to
//! every entry, honoring pre-assigned IDs (`<public>`) and the
//! `--stable-ids` map, and reporting collisions.

use crate::res::table::ResourceTable;
use crate::res::{ResourceId, ResourceName};
use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet};

pub fn assign_ids(
    table: &mut ResourceTable,
    package_id: u8,
    stable_ids: &HashMap<ResourceName, ResourceId>,
) -> Result<()> {
    for package in &mut table.packages {
        // Pre-seed from stable IDs and already-assigned (public) IDs.
        let mut used_type_ids: HashSet<u8> = HashSet::new();
        let mut used_entry_ids: HashMap<u8, HashSet<u16>> = HashMap::new();
        let mut type_id_by_name: HashMap<String, u8> = HashMap::new();

        let lookup_stable = |ty: &crate::res::ResourceNamedType, entry: &str| {
            let name = ResourceName::with_named_type(&package.name, ty.clone(), entry);
            stable_ids.get(&name).copied()
        };

        for ty in &mut package.types {
            for entry in &mut ty.entries {
                if entry.id.is_none() {
                    if let Some(stable) = lookup_stable(&ty.named_type, &entry.name) {
                        entry.id = Some(stable);
                    }
                }
                if let Some(id) = entry.id {
                    if id.package_id() != package_id {
                        // Stable IDs from another package ID are ignored,
                        // matching aapt2's behavior of keying on name only.
                        if stable_ids.is_empty() {
                            bail!(
                                "resource {}:{}/{} has ID {id} assigned for package ID {:#04x} \
                                 but the build is using package ID {package_id:#04x}",
                                package.name,
                                ty.named_type,
                                entry.name,
                                id.package_id()
                            );
                        }
                    }
                    if let Some(&existing) = type_id_by_name.get(&ty.named_type.name) {
                        if existing != id.type_id() {
                            bail!(
                                "type '{}' was assigned conflicting type IDs 0x{existing:02x} and \
                                 0x{:02x}",
                                ty.named_type,
                                id.type_id()
                            );
                        }
                    } else {
                        type_id_by_name.insert(ty.named_type.name.clone(), id.type_id());
                        used_type_ids.insert(id.type_id());
                    }
                    let entries = used_entry_ids.entry(id.type_id()).or_default();
                    if !entries.insert(id.entry_id()) {
                        bail!(
                            "resource {}:{}/{} has duplicate ID {id}",
                            package.name,
                            ty.named_type,
                            entry.name
                        );
                    }
                }
            }
        }

        // Assign IDs to types in sorted (declaration) order, then to
        // entries in name order — matching the partitioned view aapt2
        // assigns from.
        let mut type_order: Vec<usize> = (0..package.types.len()).collect();
        type_order.sort_by(|&a, &b| {
            package.types[a].named_type.cmp(&package.types[b].named_type)
        });

        let mut next_type_id: u8 = 1;
        for type_index in type_order {
            let ty = &mut package.types[type_index];
            let type_id = match type_id_by_name.get(&ty.named_type.name) {
                Some(&id) => id,
                None => {
                    while used_type_ids.contains(&next_type_id) {
                        next_type_id = next_type_id.checked_add(1).ok_or_else(|| {
                            anyhow::anyhow!("exceeded the maximum number of resource types")
                        })?;
                    }
                    let id = next_type_id;
                    used_type_ids.insert(id);
                    type_id_by_name.insert(ty.named_type.name.clone(), id);
                    id
                }
            };

            let used_entries = used_entry_ids.entry(type_id).or_default();
            let mut next_entry_id: u32 = 0;
            let mut entry_order: Vec<usize> = (0..ty.entries.len()).collect();
            entry_order.sort_by(|&a, &b| ty.entries[a].name.cmp(&ty.entries[b].name));
            for entry_index in entry_order {
                let entry = &mut ty.entries[entry_index];
                if entry.id.is_some() {
                    continue;
                }
                while used_entries.contains(&(next_entry_id as u16)) {
                    next_entry_id += 1;
                }
                if next_entry_id > u16::MAX as u32 {
                    bail!(
                        "exceeded the maximum number of resources in type '{}'",
                        ty.named_type
                    );
                }
                used_entries.insert(next_entry_id as u16);
                entry.id = Some(ResourceId::new(package_id, type_id, next_entry_id as u16));
                next_entry_id += 1;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::res::config::ConfigDescription;
    use crate::res::value::{Item, Value};
    use crate::res::ResourceType;

    #[test]
    fn assigns_sequential_ids() {
        let mut table = ResourceTable::new();
        for (ty, name) in [
            (ResourceType::String, "b"),
            (ResourceType::String, "a"),
            (ResourceType::Drawable, "icon"),
        ] {
            table
                .add_value(
                    ResourceName::new("com.app", ty, name),
                    ConfigDescription::default(),
                    Value::item(Item::Id),
                )
                .unwrap();
        }
        assign_ids(&mut table, 0x7f, &HashMap::new()).unwrap();

        let id_of = |ty, name: &str| {
            table
                .find_resource(&ResourceName::new("com.app", ty, name))
                .unwrap()
                .entry
                .id
                .unwrap()
        };
        // drawable sorts before string in type order.
        assert_eq!(id_of(ResourceType::Drawable, "icon"), ResourceId(0x7f010000));
        assert_eq!(id_of(ResourceType::String, "a"), ResourceId(0x7f020000));
        assert_eq!(id_of(ResourceType::String, "b"), ResourceId(0x7f020001));
    }

    #[test]
    fn honors_stable_ids() {
        let mut table = ResourceTable::new();
        table
            .add_value(
                ResourceName::new("com.app", ResourceType::String, "a"),
                ConfigDescription::default(),
                Value::item(Item::Id),
            )
            .unwrap();
        table
            .add_value(
                ResourceName::new("com.app", ResourceType::String, "b"),
                ConfigDescription::default(),
                Value::item(Item::Id),
            )
            .unwrap();
        let mut stable = HashMap::new();
        stable.insert(
            ResourceName::new("com.app", ResourceType::String, "b"),
            ResourceId(0x7f040005),
        );
        assign_ids(&mut table, 0x7f, &stable).unwrap();
        let b = table
            .find_resource(&ResourceName::new("com.app", ResourceType::String, "b"))
            .unwrap()
            .entry
            .id
            .unwrap();
        assert_eq!(b, ResourceId(0x7f040005));
        let a = table
            .find_resource(&ResourceName::new("com.app", ResourceType::String, "a"))
            .unwrap()
            .entry
            .id
            .unwrap();
        assert_eq!(a.type_id(), 0x04);
        assert_eq!(a.entry_id(), 0);
    }
}
