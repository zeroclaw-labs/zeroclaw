#[allow(clippy::module_inception)]
pub mod agent;
pub mod dispatcher;
pub mod loop_;
pub mod memory_loader;
pub mod prompt;

#[allow(unused_imports)]
pub use agent::{Agent, AgentBuilder};
pub use loop_::{process_message, run};

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reexport_exists<F>(_value: F) {}

    #[test]
    fn run_function_is_reexported() {
        assert_reexport_exists(run);
        assert_reexport_exists(process_message);
        assert_reexport_exists(loop_::run);
        assert_reexport_exists(loop_::process_message);
    }
}
