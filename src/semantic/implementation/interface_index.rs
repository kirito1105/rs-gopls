use super::{
    Location,
    method_sets::{InterfaceMethods, MethodShape},
};
use std::{
    collections::{HashMap, HashSet},
    rc::Rc,
};

pub(super) struct IndexedInterface {
    pub(super) location: Location,
    pub(super) methods: InterfaceMethods,
}

#[derive(Default)]
pub(super) struct InterfaceIndex {
    by_method: HashMap<MethodShape, Vec<Rc<IndexedInterface>>>,
}

impl InterfaceIndex {
    pub(super) fn insert(&mut self, location: Location, methods: InterfaceMethods) {
        // Each indexed method is mandatory, including those supplied by an embedded interface.
        let Some(anchor) = methods.method_shapes().max_by(|a, b| {
            (a.name.len(), &a.name, a.parameters, a.results, a.variadic).cmp(&(
                b.name.len(),
                &b.name,
                b.parameters,
                b.results,
                b.variadic,
            ))
        }) else {
            return;
        };
        self.by_method
            .entry(anchor)
            .or_default()
            .push(Rc::new(IndexedInterface { location, methods }));
    }

    pub(super) fn candidates(
        &self,
        shapes: Option<&HashSet<MethodShape>>,
    ) -> Vec<Rc<IndexedInterface>> {
        if let Some(shapes) = shapes {
            shapes
                .iter()
                .filter_map(|shape| self.by_method.get(shape))
                .flatten()
                .cloned()
                .collect()
        } else {
            self.by_method.values().flatten().cloned().collect()
        }
    }
}
