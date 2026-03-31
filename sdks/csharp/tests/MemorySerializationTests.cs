using System.Text.Json;
using Xunit;

namespace ZeroClaw.PluginSdk.Tests;

/// <summary>
/// Validates that Memory request/response JSON serialization matches the
/// Rust SDK wire format (snake_case keys, same field names and structure).
/// These tests exercise the serialization logic without requiring a WASM host.
/// </summary>
public class MemorySerializationTests
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        PropertyNameCaseInsensitive = true,
    };

    // -- Store request/response wire format --------------------------------

    public sealed class StoreRequest
    {
        public string Key { get; set; } = "";
        public string Value { get; set; } = "";
    }

    public sealed class StoreResponse
    {
        public bool Success { get; set; }
        public string? Error { get; set; }
    }

    [Fact]
    public void StoreRequest_Serializes_SnakeCase()
    {
        var req = new StoreRequest { Key = "user_state", Value = "some_data" };
        var json = JsonSerializer.Serialize(req, JsonOptions);

        Assert.Contains("\"key\"", json);
        Assert.Contains("\"value\"", json);
        Assert.Contains("\"user_state\"", json);
        Assert.Contains("\"some_data\"", json);
        // Must NOT contain PascalCase
        Assert.DoesNotContain("\"Key\"", json);
        Assert.DoesNotContain("\"Value\"", json);
    }

    [Fact]
    public void StoreRequest_MatchesRustWireFormat()
    {
        var req = new StoreRequest { Key = "greeting", Value = "hello world" };
        var json = JsonSerializer.Serialize(req, JsonOptions);
        var parsed = JsonDocument.Parse(json);
        var root = parsed.RootElement;

        Assert.Equal("greeting", root.GetProperty("key").GetString());
        Assert.Equal("hello world", root.GetProperty("value").GetString());
        // Exactly two properties
        Assert.Equal(2, root.EnumerateObject().Count());
    }

    [Fact]
    public void StoreResponse_Deserializes_Success()
    {
        var json = """{"success": true}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<StoreResponse>(json, JsonOptions)!;

        Assert.True(resp.Success);
        Assert.Null(resp.Error);
    }

    [Fact]
    public void StoreResponse_Deserializes_Error()
    {
        var json = """{"error": "permission denied"}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<StoreResponse>(json, JsonOptions)!;

        Assert.False(resp.Success);
        Assert.Equal("permission denied", resp.Error);
    }

    // -- Recall request/response wire format --------------------------------

    public sealed class RecallRequest
    {
        public string Query { get; set; } = "";
    }

    public sealed class RecallResponse
    {
        public string Results { get; set; } = "";
        public string? Error { get; set; }
    }

    [Fact]
    public void RecallRequest_Serializes_SnakeCase()
    {
        var req = new RecallRequest { Query = "plugin:my_plugin:" };
        var json = JsonSerializer.Serialize(req, JsonOptions);

        Assert.Contains("\"query\"", json);
        Assert.Contains("\"plugin:my_plugin:\"", json);
        Assert.DoesNotContain("\"Query\"", json);
    }

    [Fact]
    public void RecallRequest_MatchesRustWireFormat()
    {
        var req = new RecallRequest { Query = "test query" };
        var json = JsonSerializer.Serialize(req, JsonOptions);
        var parsed = JsonDocument.Parse(json);
        var root = parsed.RootElement;

        Assert.Equal("test query", root.GetProperty("query").GetString());
        Assert.Single(root.EnumerateObject());
    }

    [Fact]
    public void RecallResponse_Deserializes_WithResults()
    {
        var json = """{"results": "[{\"key\":\"a\",\"content\":\"b\"}]"}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<RecallResponse>(json, JsonOptions)!;

        Assert.Contains("key", resp.Results);
        Assert.Null(resp.Error);
    }

    [Fact]
    public void RecallResponse_Deserializes_Error()
    {
        var json = """{"error": "not found"}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<RecallResponse>(json, JsonOptions)!;

        Assert.Equal("", resp.Results);
        Assert.Equal("not found", resp.Error);
    }

    [Fact]
    public void RecallResponse_Deserializes_EmptyResults()
    {
        var json = """{"results": ""}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<RecallResponse>(json, JsonOptions)!;

        Assert.Equal("", resp.Results);
        Assert.Null(resp.Error);
    }

    // -- Forget request/response wire format --------------------------------

    public sealed class ForgetRequest
    {
        public string Key { get; set; } = "";
    }

    public sealed class ForgetResponse
    {
        public bool Success { get; set; }
        public string? Error { get; set; }
    }

    [Fact]
    public void ForgetRequest_Serializes_SnakeCase()
    {
        var req = new ForgetRequest { Key = "user_state" };
        var json = JsonSerializer.Serialize(req, JsonOptions);

        Assert.Contains("\"key\"", json);
        Assert.Contains("\"user_state\"", json);
        Assert.DoesNotContain("\"Key\"", json);
    }

    [Fact]
    public void ForgetRequest_MatchesRustWireFormat()
    {
        var req = new ForgetRequest { Key = "old_data" };
        var json = JsonSerializer.Serialize(req, JsonOptions);
        var parsed = JsonDocument.Parse(json);
        var root = parsed.RootElement;

        Assert.Equal("old_data", root.GetProperty("key").GetString());
        Assert.Single(root.EnumerateObject());
    }

    [Fact]
    public void ForgetResponse_Deserializes_Success()
    {
        var json = """{"success": true}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<ForgetResponse>(json, JsonOptions)!;

        Assert.True(resp.Success);
        Assert.Null(resp.Error);
    }

    [Fact]
    public void ForgetResponse_Deserializes_Failure()
    {
        var json = """{"success": false}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<ForgetResponse>(json, JsonOptions)!;

        Assert.False(resp.Success);
        Assert.Null(resp.Error);
    }

    [Fact]
    public void ForgetResponse_Deserializes_Error()
    {
        var json = """{"error": "key not found"}"""u8.ToArray();
        var resp = JsonSerializer.Deserialize<ForgetResponse>(json, JsonOptions)!;

        Assert.Equal("key not found", resp.Error);
    }

    // -- PluginException tests ---------------------------------------------

    [Fact]
    public void PluginException_PreservesMessage()
    {
        var ex = new PluginException("host error: timeout");
        Assert.Equal("host error: timeout", ex.Message);
        Assert.IsType<PluginException>(ex);
        Assert.IsAssignableFrom<Exception>(ex);
    }

    [Fact]
    public void PluginException_PreservesInnerException()
    {
        var inner = new InvalidOperationException("inner");
        var ex = new PluginException("wrapper", inner);
        Assert.Equal("wrapper", ex.Message);
        Assert.Same(inner, ex.InnerException);
    }
}
