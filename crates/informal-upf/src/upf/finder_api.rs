//! Design object query interface (`ObjectFinder`) for `find_objects` and hierarchy validation.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::ir::SupplyDirection;

/// Object types queried by `find_objects`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectType {
    Inst,
    Cell,
    Port,
    Net,
    Pin,
    Reg,
    Mod,
    SupplyPort,
    SupplyNet,
    SupplySet,
    PowerDomain,
}

impl ObjectType {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inst" | "instance" => Ok(Self::Inst),
            "cell" => Ok(Self::Cell),
            "port" => Ok(Self::Port),
            "net" => Ok(Self::Net),
            "pin" => Ok(Self::Pin),
            "reg" | "flop" | "latch" | "register" => Ok(Self::Reg),
            "mod" | "module" | "model" => Ok(Self::Mod),
            "supply_port" => Ok(Self::SupplyPort),
            "supply_net" => Ok(Self::SupplyNet),
            "supply_set" => Ok(Self::SupplySet),
            "power_domain" | "domain" => Ok(Self::PowerDomain),
            _ => Err(format!("find_objects: unsupported -object_type `{s}`")),
        }
    }
}

/// Read-only design query interface for `find_objects` and hierarchy validation.
///
/// Stage 1 UPF-only analysis uses [`NullObjectFinder`]; WS3+ binds a live
/// implementation backed by the elaborated SystemVerilog design.
pub trait ObjectFinder: Send + Sync {
    /// True if an RTL/netlist design is loaded (`false` in UPF-only mode).
    fn has_design(&self) -> bool;

    /// True if `scope_path` (normalized with `/`, `""` for root) exists in the design.
    fn scope_exists(&self, scope_path: &str) -> bool;

    /// Returns design objects matching `patterns` under `scope_path`.
    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String>;

    /// Returns design objects matching `patterns` and optional port/pin `direction`
    /// under `scope_path`. Defaults to [`Self::find_objects`].
    fn find_objects_with_direction(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
        _direction: Option<SupplyDirection>,
    ) -> Vec<String> {
        self.find_objects(scope_path, patterns, obj_type, transitive)
    }

    /// True if signal/port/pin `base_path` exists in the design and bit/part-select
    /// `[msb:lsb]` lies within its declared bounds.
    fn signal_slice_exists(&self, _base_path: &str, _msb: i128, _lsb: i128) -> bool {
        false
    }
}

/// Empty [`ObjectFinder`] for standalone Stage 1 UPF/CPF parsing without RTL.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullObjectFinder;

impl ObjectFinder for NullObjectFinder {
    fn has_design(&self) -> bool {
        false
    }

    fn scope_exists(&self, _scope_path: &str) -> bool {
        true
    }

    fn find_objects(
        &self,
        _scope_path: &str,
        _patterns: &[String],
        _obj_type: Option<ObjectType>,
        _transitive: bool,
    ) -> Vec<String> {
        Vec::new()
    }
}

impl<T: ObjectFinder + ?Sized> ObjectFinder for Arc<T> {
    fn has_design(&self) -> bool {
        (**self).has_design()
    }

    fn scope_exists(&self, scope_path: &str) -> bool {
        (**self).scope_exists(scope_path)
    }

    fn find_objects(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
    ) -> Vec<String> {
        (**self).find_objects(scope_path, patterns, obj_type, transitive)
    }

    fn find_objects_with_direction(
        &self,
        scope_path: &str,
        patterns: &[String],
        obj_type: Option<ObjectType>,
        transitive: bool,
        direction: Option<SupplyDirection>,
    ) -> Vec<String> {
        (**self).find_objects_with_direction(scope_path, patterns, obj_type, transitive, direction)
    }

    fn signal_slice_exists(&self, base_path: &str, msb: i128, lsb: i128) -> bool {
        (**self).signal_slice_exists(base_path, msb, lsb)
    }
}
