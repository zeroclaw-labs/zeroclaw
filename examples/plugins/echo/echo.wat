(module
  (memory (export "memory") 1)
  (global $heap (mut i32) (i32.const 1024))

  ;; ABI: alloc(len) -> ptr
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    global.get $heap
    local.set $ptr
    global.get $heap
    local.get $len
    i32.add
    global.set $heap
    local.get $ptr
  )

  ;; ABI: dealloc(ptr, len) -> ()
  ;; no-op bump allocator example
  (func (export "dealloc") (param $ptr i32) (param $len i32))

  ;; Writes a static response into memory and returns packed ptr/len in i64.
  (func $write_static_response (param $src i32) (param $len i32) (result i64)
    (local $out_ptr i32)
    ;; output text: "ok"
    (local.set $out_ptr (call 0 (i32.const 2)))
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 0)) (i32.const 111))
    (i32.store8 (i32.add (local.get $out_ptr) (i32.const 1)) (i32.const 107))
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $out_ptr)) (i64.const 32))
      (i64.extend_i32_u (i32.const 2))
    )
  )

  ;; ABI: zeroclaw_tool_execute(input_ptr, input_len) -> packed ptr/len i64
  (func (export "zeroclaw_tool_execute") (param $ptr i32) (param $len i32) (result i64)
    (call $write_static_response (local.get $ptr) (local.get $len))
  )

  ;; ABI: zeroclaw_provider_chat(input_ptr, input_len) -> packed ptr/len i64
  (func (export "zeroclaw_provider_chat") (param $ptr i32) (param $len i32) (result i64)
    (call $write_static_response (local.get $ptr) (local.get $len))
  )
)
