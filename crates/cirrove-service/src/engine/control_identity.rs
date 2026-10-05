use super::*;

impl Engine {
    pub(crate) fn renew_control_incarnation(&self) {
        *self
            .control_incarnation
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = uuid::Uuid::new_v4().to_string();
    }

    pub(crate) fn control_identity(&self, scope: &Scope, node: &Node) -> crate::PathIdentity {
        crate::PathIdentity {
            mount: self
                .control_incarnation
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            scope: scope.clone(),
            item: node.id.clone(),
        }
    }

    pub(crate) fn matches_control_identity(
        &self,
        expected: Option<&crate::PathIdentity>,
        scope: &Scope,
        node: &Node,
    ) -> bool {
        expected.is_none_or(|expected| {
            !self.cancel.is_cancelled() && *expected == self.control_identity(scope, node)
        })
    }
}
