//! Test switch for the packet journey target, used by the hash-free tunnel datagram tests.
#[cfg(test)]
pub(crate) mod journey_override {
    extern crate std;
    std::thread_local! {
        static FORCED_OFF: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
    }
    pub(crate) fn forced_off() -> bool {
        FORCED_OFF.with(|c| c.get())
    }
    /// Pins the journey target off on this thread until dropped.
    pub(crate) struct JourneyOff;
    impl JourneyOff {
        pub(crate) fn new() -> Self {
            FORCED_OFF.with(|c| c.set(true));
            JourneyOff
        }
    }
    impl Drop for JourneyOff {
        fn drop(&mut self) {
            FORCED_OFF.with(|c| c.set(false));
        }
    }
}
