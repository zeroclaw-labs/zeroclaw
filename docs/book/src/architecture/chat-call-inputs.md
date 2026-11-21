# Chat call inputs

`ChatRequest::new` and its tool/thinking methods construct request context.
`CallSampling::Explicit(None)` tells the API layer to forward `None`; whether a
provider omits the field on the wire remains the provider implementation's
responsibility.
Candidate-specific defaults use a configuration-independent resolver, supplied
by the caller; the API crate never reads configuration.

Sampling-aware methods preserve the existing provider implementation methods
and provide default forwarding. Composite providers implement candidate-aware
forwarding when the registry construction layer is enabled. This layer does
not switch runtime consumers or change response normalization.
