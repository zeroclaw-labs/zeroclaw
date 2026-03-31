namespace ZeroClaw.PluginSdk;

/// <summary>
/// Thrown when a host function call fails or returns an error response.
/// </summary>
public class PluginException : Exception
{
    public PluginException(string message) : base(message) { }

    public PluginException(string message, Exception innerException)
        : base(message, innerException) { }
}
