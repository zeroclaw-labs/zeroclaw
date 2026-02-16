pub mod loop_;

pub use loop_::run;

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_reexport_exists<F>(_value: F) {}

    #[test]
    fn run_function_is_reexported() {
        assert_reexport_exists(run);
        assert_reexport_exists(loop_::run);
    }
}
