# Onboarding flow — surface-neutral strings.
#
# Served to every interface (CLI, RPC, web). The flow carries message ids and
# args as data; the interfacing surface resolves them against the active locale.

# Locale selector — the first step of the flow.
onboard-flow-locale-prompt = Choose a language
onboard-flow-locale-confirmed = Language set to {$label}.

# Section walk outcomes.
onboard-flow-completed = Configured {$items}.
onboard-flow-cancelled = Onboarding cancelled. Nothing was changed.
onboard-flow-failed = Could not configure {$layer}:{$instance}: {$reason}

# Errors.
onboard-flow-no-fields = The section {$section} has no configurable fields.
