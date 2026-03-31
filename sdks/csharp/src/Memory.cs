using System.Runtime.InteropServices;
using System.Text.Json;
using System.Text.Json.Serialization;
using Extism;

namespace ZeroClaw.PluginSdk;

/// <summary>
/// Provides access to ZeroClaw's memory subsystem from plugin code.
/// Each method calls the corresponding host function via Extism shared memory,
/// marshalling JSON with System.Text.Json matching the Rust SDK wire format.
/// </summary>
public static class Memory
{
    // -----------------------------------------------------------------------
    // Host function imports (rewritten by Extism MSBuild at compile time)
    // -----------------------------------------------------------------------

    [DllImport("extism", EntryPoint = "zeroclaw_memory_store")]
    private static extern ulong zeroclaw_memory_store(ulong input);

    [DllImport("extism", EntryPoint = "zeroclaw_memory_recall")]
    private static extern ulong zeroclaw_memory_recall(ulong input);

    [DllImport("extism", EntryPoint = "zeroclaw_memory_forget")]
    private static extern ulong zeroclaw_memory_forget(ulong input);

    // -----------------------------------------------------------------------
    // JSON options — snake_case to match Rust SDK wire format
    // -----------------------------------------------------------------------

    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        PropertyNameCaseInsensitive = true,
    };

    // -----------------------------------------------------------------------
    // Request / response types (mirror the Rust SDK structs)
    // -----------------------------------------------------------------------

    private sealed class StoreRequest
    {
        public string Key { get; set; } = "";
        public string Value { get; set; } = "";
    }

    private sealed class StoreResponse
    {
        public bool Success { get; set; }
        public string? Error { get; set; }
    }

    private sealed class RecallRequest
    {
        public string Query { get; set; } = "";
    }

    private sealed class RecallResponse
    {
        public string Results { get; set; } = "";
        public string? Error { get; set; }
    }

    private sealed class ForgetRequest
    {
        public string Key { get; set; } = "";
    }

    private sealed class ForgetResponse
    {
        public bool Success { get; set; }
        public string? Error { get; set; }
    }

    // -----------------------------------------------------------------------
    // Public API
    // -----------------------------------------------------------------------

    /// <summary>
    /// Store a key-value pair in the agent's memory.
    /// </summary>
    /// <param name="key">Memory key (auto-prefixed by the host with the plugin namespace).</param>
    /// <param name="value">Value to persist.</param>
    /// <exception cref="PluginException">Thrown when the host reports an error.</exception>
    public static void Store(string key, string value)
    {
        var request = new StoreRequest { Key = key, Value = value };
        var response = CallHostFunction<StoreRequest, StoreResponse>(
            zeroclaw_memory_store, request);

        if (response.Error is not null)
            throw new PluginException(response.Error);
        if (!response.Success)
            throw new PluginException("memory store returned success=false");
    }

    /// <summary>
    /// Recall memories matching the given query string.
    /// </summary>
    /// <param name="query">Query string to search for in memory.</param>
    /// <returns>Raw JSON results string from the host.</returns>
    /// <exception cref="PluginException">Thrown when the host reports an error.</exception>
    public static string Recall(string query)
    {
        var request = new RecallRequest { Query = query };
        var response = CallHostFunction<RecallRequest, RecallResponse>(
            zeroclaw_memory_recall, request);

        if (response.Error is not null)
            throw new PluginException(response.Error);

        return response.Results;
    }

    /// <summary>
    /// Forget (delete) a memory entry by key.
    /// </summary>
    /// <param name="key">Memory key to delete.</param>
    /// <exception cref="PluginException">Thrown when the host reports an error.</exception>
    public static void Forget(string key)
    {
        var request = new ForgetRequest { Key = key };
        var response = CallHostFunction<ForgetRequest, ForgetResponse>(
            zeroclaw_memory_forget, request);

        if (response.Error is not null)
            throw new PluginException(response.Error);
        if (!response.Success)
            throw new PluginException("memory forget returned success=false");
    }

    // -----------------------------------------------------------------------
    // Shared host-call helper
    // -----------------------------------------------------------------------

    private static TResponse CallHostFunction<TRequest, TResponse>(
        Func<ulong, ulong> hostFn, TRequest request)
    {
        var inputBytes = JsonSerializer.SerializeToUtf8Bytes(request, JsonOptions);
        using var inputBlock = Pdk.Allocate(inputBytes);

        var outputOffset = hostFn(inputBlock.Offset);

        using var outputBlock = MemoryBlock.Find(outputOffset);
        if (outputBlock.IsEmpty)
            throw new PluginException("host function returned empty response");

        var outputBytes = outputBlock.ReadBytes();
        return JsonSerializer.Deserialize<TResponse>(outputBytes, JsonOptions)
            ?? throw new PluginException("failed to deserialize host response");
    }
}
