# ZeroClaw NixOS Module

## Usage

Add the flake input and import the module:

```nix
# flake.nix
inputs.zeroclaw.url = "github:zeroclaw-labs/zeroclaw";

outputs = { zeroclaw, ... }: {
  nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
    modules = [
      zeroclaw.nixosModules.zeroclaw
      ./zeroclaw.nix
    ];
  };
};
```

## Mutable config

By default the module regenerates `config.toml` from your NixOS settings on every service start (fully declarative).

Set `mutableConfig = true` to write `config.toml` only on first start. Subsequent restarts leave the file untouched, so changes made through the web UI or CLI persist across restarts.

```nix
services.zeroclaw = {
  enable = true;
  mutableConfig = true;
  # ...
};
```

To reset back to the NixOS-managed config, delete `${stateDir}/config.toml` (default: `/var/lib/zeroclaw/config.toml`) and restart the service.

## Example: Telegram bot with Ollama

```nix
# zeroclaw.nix
{ config, ... }:
{
  sops.secrets.telegram-bot-token = { };
  sops.secrets.anthropic-api-key = { };

  services.zeroclaw = {
    enable = true;

    channels.telegram.secretFiles.bot_token = config.sops.secrets.telegram-bot-token.path;

    agents = {
      researcher.apiKeyFile = config.sops.secrets.anthropic-api-key.path;
      coder.apiKeyFile = config.sops.secrets.anthropic-api-key.path;
    };

    settings = {
      default_provider = "ollama";
      default_model = "qwen3-coder-next:q4_K_M";
      api_url = "http://localhost:11434";
      channels_config.telegram.allowed_users = [ "your-username" ];
      autonomy.auto_approve = [ "file_read" "memory_recall" "web_fetch" "web_search" ];

      agents = {
        researcher = {
          provider = "anthropic";
          model = "claude-sonnet-4-6";
          system_prompt = "You are a research assistant.";
        };
        coder = {
          provider = "anthropic";
          model = "claude-sonnet-4-6";
          system_prompt = "You are a coding assistant.";
        };
      };
    };
  };
}
```

Secrets are read from files at service start and never written to the Nix store.
Non-secret settings go in `settings` and are serialised 1:1 to TOML.
