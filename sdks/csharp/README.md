# ZeroClaw.PluginSdk (C#)

C# SDK for building ZeroClaw WASM plugins with Extism .NET PDK.

## Quickstart

### Prerequisites

- [.NET 8 SDK](https://dotnet.microsoft.com/download/dotnet/8.0)
- WASI workload: `dotnet workload install wasi-experimental`

### Project setup

Reference the SDK project from your plugin `.csproj`:

```xml
<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <TargetFramework>net8.0</TargetFramework>
    <RuntimeIdentifier>wasi-wasm</RuntimeIdentifier>
    <OutputType>Library</OutputType>
    <Nullable>enable</Nullable>
  </PropertyGroup>
  <ItemGroup>
    <ProjectReference Include="path/to/sdks/csharp/ZeroClaw.PluginSdk.csproj" />
  </ItemGroup>
</Project>
```

The SDK already includes `Extism.Pdk` as a transitive dependency.

### Write a plugin

Mark entry-point methods with `[PluginFunction]` and use `PluginEntryPoint.Invoke` to handle JSON marshalling:

```csharp
using Extism;
using ZeroClaw.PluginSdk;

public record GreetInput(string Name);
public record GreetOutput(string Message);

public static class GreetPlugin
{
    [PluginFunction("greet")]
    public static int Greet()
    {
        return PluginEntryPoint.Invoke<GreetInput, GreetOutput>(input =>
            new GreetOutput($"Hello, {input.Name}!"));
    }
}
```

`PluginEntryPoint.Invoke`:
- Reads JSON input from Extism shared memory
- Deserializes to the input type (using `snake_case` naming)
- Calls your handler and serializes the result back
- Returns `0` on success, `1` on error (with an error JSON payload)

### Build to WASM

```bash
dotnet build -r wasi-wasm -c Release
```

The compiled `.wasm` file will be in `bin/Release/net8.0/wasi-wasm/`.

### Run tests

```bash
cd sdks/csharp/tests
dotnet test
```
