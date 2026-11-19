# Chat call inputs

`ChatRequest::new` and its tool/thinking methods construct request context.
`CallSampling::Explicit(None)` means omit temperature on the wire.
Candidate-specific defaults use a configuration-independent resolver, supplied
by the caller; the API crate never reads configuration.

Sampling-aware methods preserve the existing provider implementation methods
and provide default forwarding. Composite providers implement candidate-aware
forwarding when the registry construction layer is enabled. This layer does
not switch runtime consumers or change response normalization.
