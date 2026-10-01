//! Owned validator wrapper around the reusable PACKAGE wire transport.
use super::*;
pub use crate::package_transport::PackageAllocationRefusal;
pub(super) use crate::package_transport::{Slot, upload};
pub(super) async fn allocate(
    session: &mut ICloudReadSession,
    plan: &OwnedPackagePlan,
) -> Result<Slot> {
    crate::package_transport::allocate(session, &plan.destination, plan.archive_size).await
}
fn registration(saved: &Checkpoint) -> Result<serde_json::Value> {
    ensure!(
        saved.phase == Phase::RegistrationStarted,
        "package registration not durably armed"
    );
    saved.plan.validate()?;
    let slot = saved.slot.as_ref().context("package slot absent")?;
    ensure!(
        slot.document_id != saved.plan.source.docwsid,
        "package allocation reused source"
    );
    crate::package_transport::registration(
        &saved.plan.parent,
        &saved.plan.destination,
        slot,
        saved
            .registration
            .as_deref()
            .context("package receipt absent")?,
    )
}
pub(super) async fn register(session: &mut ICloudReadSession, saved: &Checkpoint) -> Result<()> {
    registration(saved)?;
    crate::package_transport::register(
        session,
        &saved.plan.parent,
        &saved.plan.destination,
        saved.slot.as_ref().context("package slot absent")?,
        saved
            .registration
            .as_deref()
            .context("package receipt absent")?,
    )
    .await
}
