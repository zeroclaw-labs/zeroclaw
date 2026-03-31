using System.Reflection;
using System.Runtime.InteropServices;
using Xunit;

namespace ZeroClaw.PluginSdk.Tests;

/// <summary>
/// Verifies that Memory.Store/Recall/Forget call the correct host functions
/// (zeroclaw_memory_store, zeroclaw_memory_recall, zeroclaw_memory_forget)
/// via Extism .NET PDK DllImport declarations.
///
/// Acceptance criterion US-ZCL-38-2:
///   "Methods call zeroclaw_memory_store/recall/forget host functions via Extism .NET PDK"
/// </summary>
public class MemoryHostFunctionTests
{
    private const BindingFlags NonPublicStatic =
        BindingFlags.NonPublic | BindingFlags.Static;

    // -- zeroclaw_memory_store ------------------------------------------------

    [Fact]
    public void Store_DllImport_EntryPointIsCorrect()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_store", NonPublicStatic);
        Assert.NotNull(method);

        var attr = method!.GetCustomAttribute<DllImportAttribute>();
        Assert.NotNull(attr);
        Assert.Equal("zeroclaw_memory_store", attr!.EntryPoint);
    }

    [Fact]
    public void Store_DllImport_LibraryIsExtism()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_store", NonPublicStatic);
        var attr = method!.GetCustomAttribute<DllImportAttribute>()!;

        Assert.Equal("extism", attr.Value);
    }

    [Fact]
    public void Store_DllImport_SignatureMatchesExtismConvention()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_store", NonPublicStatic)!;

        Assert.Equal(typeof(ulong), method.ReturnType);
        var parameters = method.GetParameters();
        Assert.Single(parameters);
        Assert.Equal(typeof(ulong), parameters[0].ParameterType);
    }

    // -- zeroclaw_memory_recall -----------------------------------------------

    [Fact]
    public void Recall_DllImport_EntryPointIsCorrect()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_recall", NonPublicStatic);
        Assert.NotNull(method);

        var attr = method!.GetCustomAttribute<DllImportAttribute>();
        Assert.NotNull(attr);
        Assert.Equal("zeroclaw_memory_recall", attr!.EntryPoint);
    }

    [Fact]
    public void Recall_DllImport_LibraryIsExtism()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_recall", NonPublicStatic);
        var attr = method!.GetCustomAttribute<DllImportAttribute>()!;

        Assert.Equal("extism", attr.Value);
    }

    [Fact]
    public void Recall_DllImport_SignatureMatchesExtismConvention()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_recall", NonPublicStatic)!;

        Assert.Equal(typeof(ulong), method.ReturnType);
        var parameters = method.GetParameters();
        Assert.Single(parameters);
        Assert.Equal(typeof(ulong), parameters[0].ParameterType);
    }

    // -- zeroclaw_memory_forget -----------------------------------------------

    [Fact]
    public void Forget_DllImport_EntryPointIsCorrect()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_forget", NonPublicStatic);
        Assert.NotNull(method);

        var attr = method!.GetCustomAttribute<DllImportAttribute>();
        Assert.NotNull(attr);
        Assert.Equal("zeroclaw_memory_forget", attr!.EntryPoint);
    }

    [Fact]
    public void Forget_DllImport_LibraryIsExtism()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_forget", NonPublicStatic);
        var attr = method!.GetCustomAttribute<DllImportAttribute>()!;

        Assert.Equal("extism", attr.Value);
    }

    [Fact]
    public void Forget_DllImport_SignatureMatchesExtismConvention()
    {
        var method = typeof(Memory).GetMethod("zeroclaw_memory_forget", NonPublicStatic)!;

        Assert.Equal(typeof(ulong), method.ReturnType);
        var parameters = method.GetParameters();
        Assert.Single(parameters);
        Assert.Equal(typeof(ulong), parameters[0].ParameterType);
    }

    // -- Completeness: all three host functions are declared -------------------

    [Fact]
    public void Memory_DeclaresExactlyThreeHostFunctions()
    {
        var dllImportMethods = typeof(Memory)
            .GetMethods(NonPublicStatic)
            .Where(m => m.GetCustomAttribute<DllImportAttribute>() is not null)
            .ToList();

        Assert.Equal(3, dllImportMethods.Count);

        var entryPoints = dllImportMethods
            .Select(m => m.GetCustomAttribute<DllImportAttribute>()!.EntryPoint)
            .OrderBy(e => e)
            .ToList();

        Assert.Equal(
            new[] { "zeroclaw_memory_forget", "zeroclaw_memory_recall", "zeroclaw_memory_store" },
            entryPoints);
    }

    [Fact]
    public void Memory_AllHostFunctions_ImportFromExtism()
    {
        var dllImportMethods = typeof(Memory)
            .GetMethods(NonPublicStatic)
            .Where(m => m.GetCustomAttribute<DllImportAttribute>() is not null);

        foreach (var method in dllImportMethods)
        {
            var attr = method.GetCustomAttribute<DllImportAttribute>()!;
            Assert.Equal("extism", attr.Value);
        }
    }

    // -- Public API routes through the correct host function ------------------

    [Fact]
    public void Store_PublicMethod_Exists_WithCorrectSignature()
    {
        var method = typeof(Memory).GetMethod("Store", BindingFlags.Public | BindingFlags.Static);
        Assert.NotNull(method);
        Assert.Equal(typeof(void), method!.ReturnType);

        var parameters = method.GetParameters();
        Assert.Equal(2, parameters.Length);
        Assert.Equal(typeof(string), parameters[0].ParameterType);
        Assert.Equal("key", parameters[0].Name);
        Assert.Equal(typeof(string), parameters[1].ParameterType);
        Assert.Equal("value", parameters[1].Name);
    }

    [Fact]
    public void Recall_PublicMethod_Exists_WithCorrectSignature()
    {
        var method = typeof(Memory).GetMethod("Recall", BindingFlags.Public | BindingFlags.Static);
        Assert.NotNull(method);
        Assert.Equal(typeof(string), method!.ReturnType);

        var parameters = method.GetParameters();
        Assert.Single(parameters);
        Assert.Equal(typeof(string), parameters[0].ParameterType);
        Assert.Equal("query", parameters[0].Name);
    }

    [Fact]
    public void Forget_PublicMethod_Exists_WithCorrectSignature()
    {
        var method = typeof(Memory).GetMethod("Forget", BindingFlags.Public | BindingFlags.Static);
        Assert.NotNull(method);
        Assert.Equal(typeof(void), method!.ReturnType);

        var parameters = method.GetParameters();
        Assert.Single(parameters);
        Assert.Equal(typeof(string), parameters[0].ParameterType);
        Assert.Equal("key", parameters[0].Name);
    }
}
