#include <string.h>
#include <mono/metadata/assembly.h>
#include <mono/metadata/exception.h>

// https://github.com/dotnet/runtime/blob/v7.0.0/src/mono/wasi/mono-wasi-driver/driver.c
#include <string.h>

#include "driver.h"

#include <stdint.h>
#include <assert.h>
#include <stdlib.h>
#include <stdbool.h>
#include <stdio.h>

#define IMPORT(a, b) __attribute__((import_module(a), import_name(b)))

typedef uint64_t ExtismPointer;

IMPORT("extism:host/env", "input_length")
extern uint64_t extism_input_length_import();

uint64_t extism_input_length() {
	return extism_input_length_import();
}

IMPORT("extism:host/env", "length")
extern uint64_t extism_length_import(ExtismPointer);

uint64_t extism_length(ExtismPointer p) {
	return extism_length_import(p);
}

IMPORT("extism:host/env", "alloc")
extern ExtismPointer extism_alloc_import(uint64_t size);

ExtismPointer extism_alloc(uint64_t size) {
	return extism_alloc_import(size);
}

IMPORT("extism:host/env", "free")
extern void extism_free_import(ExtismPointer p);

void extism_free(ExtismPointer p) {
	extism_free_import(p);
}

IMPORT("extism:host/env", "input_load_u8")
extern uint8_t extism_input_load_u8_import(ExtismPointer p);

uint8_t extism_input_load_u8(ExtismPointer p) {
	return extism_input_load_u8_import(p);
}

IMPORT("extism:host/env", "input_load_u64")
extern uint64_t extism_input_load_u64_import(ExtismPointer p);

uint64_t extism_input_load_u64(ExtismPointer p) {
	return extism_input_load_u64_import(p);
}

IMPORT("extism:host/env", "output_set")
extern void extism_output_set_import(ExtismPointer p, uint64_t value);

void extism_output_set(ExtismPointer p, uint64_t value) {
	extism_output_set_import(p, value);
}

IMPORT("extism:host/env", "error_set")
extern void extism_error_set_import(ExtismPointer p);

void extism_error_set(ExtismPointer p) {
	extism_error_set_import(p);
}

IMPORT("extism:host/env", "config_get")
extern ExtismPointer extism_config_get_import(ExtismPointer p);

ExtismPointer extism_config_get(ExtismPointer p) {
	return extism_config_get_import(p);
}

IMPORT("extism:host/env", "var_get")
extern ExtismPointer extism_var_get_import(ExtismPointer p);

ExtismPointer extism_var_get(ExtismPointer p) {
	return extism_var_get_import(p);
}

IMPORT("extism:host/env", "var_set")
extern void extism_var_set_import(ExtismPointer p1, ExtismPointer p2);

void extism_var_set(ExtismPointer p1, ExtismPointer p2) {
	extism_var_set_import(p1, p2);
}

IMPORT("extism:host/env", "store_u8")
extern void extism_store_u8_import(ExtismPointer p, uint8_t value);

void extism_store_u8(ExtismPointer p, uint8_t value) {
	extism_store_u8_import(p, value);
}

IMPORT("extism:host/env", "load_u8")
extern uint8_t extism_load_u8_import(ExtismPointer p);

uint8_t extism_load_u8(ExtismPointer p) {
	return extism_load_u8_import(p);
}

IMPORT("extism:host/env", "store_u64")
extern void extism_store_u64_import(ExtismPointer p, uint64_t value);

void extism_store_u64(ExtismPointer p, uint64_t value) {
	extism_store_u64_import(p, value);
}

IMPORT("extism:host/env", "load_u64")
extern uint64_t extism_load_u64_import(ExtismPointer p);

uint64_t extism_load_u64(ExtismPointer p) {
	return extism_load_u64_import(p);
}

IMPORT("extism:host/env", "http_request")
extern ExtismPointer extism_http_request_import(ExtismPointer p1, ExtismPointer p2);

ExtismPointer extism_http_request(ExtismPointer p1, ExtismPointer p2) {
	return extism_http_request_import(p1, p2);
}

IMPORT("extism:host/env", "http_status_code")
extern int32_t extism_http_status_code_import();

int32_t extism_http_status_code() {
	return extism_http_status_code_import();
}

IMPORT("extism:host/env", "http_headers")
extern ExtismPointer extism_http_headers_import();

ExtismPointer extism_http_headers() {
	return extism_http_headers_import();
}

// Logging functions
IMPORT("extism:host/env", "get_log_level")
extern int32_t extism_get_log_level_import();

int32_t extism_get_log_level() {
	return extism_get_log_level_import();
}

IMPORT("extism:host/env", "log_trace")
extern void extism_log_trace_import(ExtismPointer p);

void extism_log_trace(ExtismPointer p) {
	extism_log_trace_import(p);
}

IMPORT("extism:host/env", "log_debug")
extern void extism_log_debug_import(ExtismPointer p);

void extism_log_debug(ExtismPointer p) {
	extism_log_debug_import(p);
}

IMPORT("extism:host/env", "log_info")
extern void extism_log_info_import(ExtismPointer p);

void extism_log_info(ExtismPointer p) {
	extism_log_info_import(p);
}

IMPORT("extism:host/env", "log_warn")
extern void extism_log_warn_import(ExtismPointer p);

void extism_log_warn(ExtismPointer p) {
	extism_log_warn_import(p);
}

IMPORT("extism:host/env", "log_error")
extern void extism_log_error_import(ExtismPointer p);

void extism_log_error(ExtismPointer p) {
	extism_log_error_import(p);
}

void extism_load(uint64_t offs, uint8_t* buffer, uint64_t length) {
	uint64_t n;
	uint64_t left = 0;

	for (uint64_t i = 0; i < length; i += 1) {
		left = length - i;
		if (left < 8) {
			buffer[i] = extism_load_u8(offs + i);
			continue;
		}

		n = extism_load_u64(offs + i);
		*((uint64_t*)buffer + (i / 8)) = n;
		i += 7;
	}
}

void extism_load_input(uint8_t* buffer, uint64_t length) {
	uint64_t n;
	uint64_t left = 0;

	for (uint64_t i = 0; i < length; i += 1) {
		left = length - i;
		if (left < 8) {
			buffer[i] = extism_input_load_u8(i);
			continue;
		}

		n = extism_input_load_u64(i);
		*((uint64_t*)buffer + (i / 8)) = n;
		i += 7;
	}
}

void extism_store(uint64_t offs, const uint8_t* buffer, uint64_t length) {
	uint64_t n;
	uint64_t left = 0;
	for (uint64_t i = 0; i < length; i++) {
		left = length - i;
		if (left < 8) {
			extism_store_u8(offs + i, buffer[i]);
			continue;
		}

		n = *((uint64_t*)buffer + (i / 8));
		extism_store_u64(offs + i, n);
		i += 7;
	}
}

// Wrap mono_runtime_run_main so that we ensure at least one argument is passed in to Mono
// otherwise it crashes, we use the -Wl,--wrap flag to instruct the linker to replace mono_runtime_run_main with
// __wrap_mono_runtime_run_main everywhere. see:
// - build/Extism.Pdk.targets
// - https://gist.github.com/mlabbe/a0b7b14be652085341162321a0a08530
// - https://github.com/dotnet/runtime/blob/4101144c8dde177addfb93ac46425fd1a8604f7a/src/mono/mono/metadata/object.c#L4175
int __real_mono_runtime_run_main(MonoMethod* method, int argc, char* argv[], MonoObject** exc);

int __wrap_mono_runtime_run_main(MonoMethod* method, int argc, char* argv[], MonoObject** exc)
{
	if (argc == 0)
	{
		char* temp[] = { "extism", NULL };
		argv = temp;
		argc = 1;
	}

	return __real_mono_runtime_run_main(method, argc, argv, exc);
}

// Wrap mono_wasm_load_runtime to make sure we don't initialize mono more than once
void __real_mono_wasm_load_runtime(const char* unused, int debug_level);

bool mono_runtime_initialized = false;
void __wrap_mono_wasm_load_runtime(const char* unused, int debug_level) {
	if (mono_runtime_initialized) {
		return;
	}

	__real_mono_wasm_load_runtime(unused, debug_level);
	mono_runtime_initialized = true;
}