//! Static phase durations only, enabled in the explicit developer write probe.
//! No item identifiers, payloads, URLs, headers, or credentials are accepted.
#[cfg(feature = "write-probe")]
pub(crate) struct Timing {
    phase: &'static str,
    started: std::time::Instant,
}
#[cfg(feature = "write-probe")]
impl Timing {
    pub fn finish(self) {}
    pub fn start(phase: &'static str) -> Self {
        Self {
            phase,
            started: std::time::Instant::now(),
        }
    }
}
#[cfg(feature = "write-probe")]
impl Drop for Timing {
    fn drop(&mut self) {
        eprintln!(
            "iCloud phase timing: {} {:.3}s",
            self.phase,
            self.started.elapsed().as_secs_f64()
        );
    }
}
#[cfg(not(feature = "write-probe"))]
pub(crate) struct Timing;
#[cfg(not(feature = "write-probe"))]
impl Timing {
    pub fn finish(self) {}
    pub fn start(_: &'static str) -> Self {
        Self
    }
}
