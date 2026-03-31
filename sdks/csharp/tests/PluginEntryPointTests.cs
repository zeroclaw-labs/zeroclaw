using System.Text.Json;
using Xunit;
using ZeroClaw.PluginSdk;

namespace ZeroClaw.PluginSdk.Tests;

public class PluginEntryPointTests
{
    public record EchoInput(string Message, int Count);
    public record EchoOutput(string Reply, bool Ok);

    [Fact]
    public void Serialize_RoundTrips_Record()
    {
        var input = new EchoInput("hello", 3);
        var bytes = PluginEntryPoint.Serialize(input);
        var back = PluginEntryPoint.Deserialize<EchoInput>(bytes);

        Assert.Equal("hello", back.Message);
        Assert.Equal(3, back.Count);
    }

    [Fact]
    public void Serialize_Uses_SnakeCase()
    {
        var input = new EchoInput("test", 1);
        var json = System.Text.Encoding.UTF8.GetString(PluginEntryPoint.Serialize(input));

        Assert.Contains("\"message\"", json);
        Assert.Contains("\"count\"", json);
    }

    [Fact]
    public void Deserialize_EmptyBytes_ReturnsDefault()
    {
        var result = PluginEntryPoint.Deserialize<EchoInput>(Array.Empty<byte>());
        Assert.Null(result);
    }

    [Fact]
    public void Deserialize_NullBytes_ReturnsDefault()
    {
        var result = PluginEntryPoint.Deserialize<EchoInput>(null!);
        Assert.Null(result);
    }

    [Fact]
    public void Serialize_Dict_RoundTrips()
    {
        var dict = new Dictionary<string, object>
        {
            ["tool_name"] = "search",
            ["query"] = "hello",
        };
        var bytes = PluginEntryPoint.Serialize(dict);
        var back = PluginEntryPoint.Deserialize<Dictionary<string, JsonElement>>(bytes);

        Assert.Equal("search", back["tool_name"].GetString());
        Assert.Equal("hello", back["query"].GetString());
    }

    [Fact]
    public void Serialize_NestedObject_RoundTrips()
    {
        var nested = new Dictionary<string, object>
        {
            ["outer"] = new Dictionary<string, object>
            {
                ["inner"] = "value",
            },
        };
        var bytes = PluginEntryPoint.Serialize(nested);
        var json = System.Text.Encoding.UTF8.GetString(bytes);

        Assert.Contains("\"outer\"", json);
        Assert.Contains("\"inner\"", json);
        Assert.Contains("\"value\"", json);
    }

    [Fact]
    public void PluginFunctionAttribute_DefaultName_IsNull()
    {
        var attr = new PluginFunctionAttribute();
        Assert.Null(attr.Name);
    }

    [Fact]
    public void PluginFunctionAttribute_CustomName_IsPreserved()
    {
        var attr = new PluginFunctionAttribute("my_tool");
        Assert.Equal("my_tool", attr.Name);
    }
}
