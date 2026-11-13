(function() {
    var type_impls = Object.fromEntries([["zeroclaw",[]],["zeroclaw_infra",[]],["zeroclaw_runtime",[]]]);
    if (window.register_type_impls) {
        window.register_type_impls(type_impls);
    } else {
        window.pending_type_impls = type_impls;
    }
})()
//{"start":55,"fragment_lengths":[15,22,24]}