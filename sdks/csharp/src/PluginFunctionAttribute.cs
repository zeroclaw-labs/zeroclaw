using System.Text.Json;
using Extism;

namespace ZeroClaw.PluginSdk;

/// <summary>
/// Marks a static method as a ZeroClaw plugin entry point.
/// The host runtime discovers methods with this attribute and
/// routes incoming tool calls to them.
/// </summary>
[AttributeUsage(AttributeTargets.Method, AllowMultiple = false)]
public sealed class PluginFunctionAttribute : Attribute
{
    public string? Name { get; }

    public PluginFunctionAttribute(string? name = null)
    {
        Name = name;
    }
}

/// <summary>
/// Handles JSON serialization/deserialization for plugin entry points.
/// Reads JSON input from the Extism host, deserializes it, invokes the
/// plugin function, and writes the serialized JSON result back.
/// </summary>
public static class PluginEntryPoint
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        PropertyNameCaseInsensitive = true,
    };

    /// <summary>
    /// Wraps a plugin function that takes typed input and returns typed output.
    /// Handles JSON deserialization of input and serialization of output.
    /// </summary>
    public static int Invoke<TInput, TOutput>(Func<TInput, TOutput> handler)
    {
        try
        {
            var raw = Pdk.GetInput();
            var input = Deserialize<TInput>(raw);
            var result = handler(input);
            var output = Serialize(result);
            Pdk.SetOutput(output);
            return 0;
        }
        catch (Exception ex)
        {
            var error = Serialize(new { error = ex.Message, success = false });
            Pdk.SetOutput(error);
            return 1;
        }
    }

    /// <summary>
    /// Wraps a plugin function that takes typed input and returns no output.
    /// </summary>
    public static int Invoke<TInput>(Action<TInput> handler)
    {
        return Invoke<TInput, object>(input =>
        {
            handler(input);
            return new { success = true };
        });
    }

    /// <summary>
    /// Wraps a plugin function that takes no input and returns typed output.
    /// </summary>
    public static int InvokeNoInput<TOutput>(Func<TOutput> handler)
    {
        try
        {
            var result = handler();
            var output = Serialize(result);
            Pdk.SetOutput(output);
            return 0;
        }
        catch (Exception ex)
        {
            var error = Serialize(new { error = ex.Message, success = false });
            Pdk.SetOutput(error);
            return 1;
        }
    }

    /// <summary>
    /// Deserializes a JSON byte array into the specified type.
    /// Returns default(T) for null or empty input.
    /// </summary>
    public static T Deserialize<T>(byte[] data)
    {
        if (data == null || data.Length == 0)
        {
            return default!;
        }

        return JsonSerializer.Deserialize<T>(data, JsonOptions)!;
    }

    /// <summary>
    /// Serializes an object to a UTF-8 JSON byte array.
    /// </summary>
    public static byte[] Serialize<T>(T value)
    {
        return JsonSerializer.SerializeToUtf8Bytes(value, JsonOptions);
    }
}
