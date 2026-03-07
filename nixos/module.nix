{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.services.zeroclaw;
  inherit (lib)
    attrNames
    concatMapStrings
    literalExpression
    mkEnableOption
    mkIf
    mkOption
    mkPackageOption
    optionalAttrs
    optionalString
    types
    ;

  toml = pkgs.formats.toml { };

  channelsWithSecret = lib.filterAttrs (_: channel: channel.secretFiles != { }) cfg.channels;
  agentsWithSecret = attrNames (lib.filterAttrs (_: agent: agent.apiKeyFile != null) cfg.agents);
  nonSecretChannelNames = attrNames (
    lib.filterAttrs (k: _: !(channelsWithSecret ? ${k})) (cfg.settings.channels_config or { })
  );

  # Strips channels_config entirely when any channel has secrets to avoid TOML
  # duplicate-table-header errors. Strips individual agent entries that have an
  # apiKeyFile. Both sections are re-written by the mergeScript at runtime.
  filteredSettings =
    let
      settingsNoChannelsWithSecret =
        if channelsWithSecret != { } then
          lib.filterAttrs (k: _: k != "channels_config") cfg.settings
        else
          cfg.settings;
      agentsWithoutSecret = lib.filterAttrs (
        k: _: !(builtins.elem k agentsWithSecret)
      ) settingsNoChannelsWithSecret.agents;
    in
    if agentsWithSecret != [ ] && settingsNoChannelsWithSecret ? agents then
      settingsNoChannelsWithSecret // { agents = agentsWithoutSecret; }
    else
      settingsNoChannelsWithSecret;

  configFile = toml.generate "zeroclaw-config.toml" filteredSettings;

  # cli = true belongs to the parent [channels_config] table and must appear
  # before any [channels_config.*] subtables.
  channelsConfigHeader = toml.generate "zeroclaw-channels-header.toml" {
    channels_config.cli = true;
  };

  channelConfigFile =
    name:
    toml.generate "zeroclaw-channel-${name}.toml" {
      channels_config.${name} = (cfg.settings.channels_config or { }).${name} or { };
    };

  agentConfigFile =
    name:
    toml.generate "zeroclaw-agent-${name}.toml" {
      agents.${name} = (cfg.settings.agents or { }).${name} or { };
    };

  mergeScript = pkgs.writeShellScript "zeroclaw-merge-config" ''
    set -euo pipefail
    ${optionalString cfg.mutableConfig ''
      if [ -f ${cfg.stateDir}/config.toml ]; then
        echo "zeroclaw: mutableConfig is enabled and config.toml already exists, skipping update"
        exit 0
      fi
    ''}
    install -m 0600 -o ${cfg.user} -g ${cfg.group} ${configFile} ${cfg.stateDir}/config.toml
    ${optionalString (channelsWithSecret != { }) ''
      cat ${channelsConfigHeader} >> ${cfg.stateDir}/config.toml
      ${concatMapStrings (channelName: ''
        cat ${channelConfigFile channelName} >> ${cfg.stateDir}/config.toml
      '') nonSecretChannelNames}
      ${concatMapStrings (channelName: ''
        cat ${channelConfigFile channelName} >> ${cfg.stateDir}/config.toml
        ${concatMapStrings (fieldName: ''
          printf '${fieldName} = "%s"\n' "$(cat ${
            lib.escapeShellArg cfg.channels.${channelName}.secretFiles.${fieldName}
          })" >> ${cfg.stateDir}/config.toml
        '') (attrNames cfg.channels.${channelName}.secretFiles)}
      '') (attrNames channelsWithSecret)}
    ''}
    ${concatMapStrings (agentName: ''
      cat ${agentConfigFile agentName} >> ${cfg.stateDir}/config.toml
      printf 'api_key = "%s"\n' "$(cat ${
        lib.escapeShellArg cfg.agents.${agentName}.apiKeyFile
      })" >> ${cfg.stateDir}/config.toml
    '') agentsWithSecret}
  '';
in
{
  options.services.zeroclaw = {
    enable = mkEnableOption "ZeroClaw agent daemon";

    package = mkPackageOption pkgs "zeroclaw" { };

    stateDir = mkOption {
      type = types.str;
      default = "/var/lib/zeroclaw";
      description = ''
        Directory for ZeroClaw runtime state.
        The config.toml is written here at each service start.
        Use settings and the secret options rather than editing it directly.
      '';
    };

    host = mkOption {
      type = types.str;
      default = "127.0.0.1";
      description = "Host the gateway binds to.";
    };

    port = mkOption {
      type = types.port;
      default = 42617;
      description = "Port the gateway listens on.";
    };

    user = mkOption {
      type = types.str;
      default = "zeroclaw";
      description = "User account the daemon runs as.";
    };

    group = mkOption {
      type = types.str;
      default = "zeroclaw";
      description = "Group the daemon runs as.";
    };

    settings = mkOption {
      type = types.attrs;
      default = { };
      description = ''
        Non-secret configuration serialised 1:1 as TOML into config.toml.
        Keys not set here fall back to Rust-side defaults in the binary.

        This is the primary way to configure the provider, model, memory
        backend, autonomy policy, heartbeat, observability, and anything
        else not covered by a dedicated module option.

        Do not place secrets here. The result ends up in the Nix store.
        Use channels.<name>.secretFiles or agents.<name>.apiKeyFile instead.
      '';
      example = literalExpression ''
        {
          default_provider = "ollama";
          default_model = "qwen3-coder-next:q4_K_M";
          api_url = "http://gpu.wg:11434";
          memory.backend = "sqlite";
          web_fetch = { enabled = true; allowed_domains = [ "*" ]; };
          web_search.enabled = true;
          autonomy.auto_approve = [ "file_read" "memory_recall" "web_fetch" "web_search" ];
          heartbeat = { enabled = true; interval_minutes = 60; };
          observability.backend = "log";
        }
      '';
    };

    agents = mkOption {
      type = types.attrsOf (
        types.submodule {
          options.apiKeyFile = mkOption {
            type = types.nullOr types.path;
            default = null;
            description = ''
              Path to a file containing the API key for this agent's provider.
              If null, the agent uses the top-level api_key or ZEROCLAW_API_KEY.

              Non-secret agent settings (provider, model, agentic, allowed_tools,
              system_prompt, etc.) belong in settings.agents.<name>.
            '';
            example = "/run/secrets/anthropic-api-key";
          };
        }
      );
      default = { };
      description = ''
        Per-agent runtime secrets. Keys are agent names matching
        settings.agents.<name>. Only the API key goes here. All other
        agent settings belong in settings.agents.<name>.
      '';
      example = literalExpression ''
        {
          researcher.apiKeyFile = "/run/secrets/anthropic-api-key";
          coder.apiKeyFile = "/run/secrets/ollama-api-key";
        }
      '';
    };

    environmentFile = mkOption {
      type = types.nullOr types.path;
      default = null;
      description = ''
        Path to a file of environment variables injected into the daemon.
        Useful for secrets that have environment-variable overrides, most
        notably ZEROCLAW_API_KEY (the main provider API key). Channel
        tokens have no env-var overrides: use channels.<name>.secretFiles.

        Example file contents:
          ZEROCLAW_API_KEY=sk-ant-...
      '';
    };

    mutableConfig = mkOption {
      type = types.bool;
      default = false;
      description = ''
        When false (default), the config.toml is regenerated from module options and
        settings on every service start.

        When true, config.toml is written only on first start (if it does not
        already exist). Subsequent starts leave the file untouched, so runtime
        edits made through the web UI or CLI persist across restarts.

        Switching from false to true takes effect on the next start after the
        file is first written. To reset to the Nix-managed config, delete
        ''${cfg.stateDir}/config.toml and restart the service.
      '';
    };

    channels = mkOption {
      type = types.attrsOf (
        types.submodule {
          options.secretFiles = mkOption {
            type = types.attrsOf types.path;
            default = { };
            description = ''
              Map of TOML field name → path to a file containing that secret value.
              Read at service start; never written to the Nix store.
              Having at least one entry here enables the channel.

              Field names must match the zeroclaw config keys exactly (e.g.
              bot_token, app_token, access_token). Works with any secrets
              manager (sops-nix, agenix, plain files, …).

              Non-secret channel settings (allowed_users, stream_mode, etc.)
              belong in settings.channels_config.<name>.
            '';
            example = literalExpression ''
              { bot_token = "/run/secrets/telegram-bot-token"; }
            '';
          };
        }
      );
      default = { };
      description = ''
        Per-channel runtime secrets. Keys are channel names as recognised by
        zeroclaw (telegram, discord, slack, mattermost, matrix, …).

        Each channel's secretFiles maps TOML field names to secret file paths.
        Non-secret settings belong in settings.channels_config.<name>.

        Channels with no secrets (e.g. signal, cli) need no entry here:
        configure them entirely via settings.channels_config.<name>.
      '';
      example = literalExpression ''
        {
          telegram.secretFiles.bot_token = "/run/secrets/telegram-bot-token";
          slack.secretFiles = {
            bot_token = "/run/secrets/slack-bot-token";
            app_token = "/run/secrets/slack-app-token";
          };
          matrix.secretFiles.access_token = "/run/secrets/matrix-access-token";
        }
      '';
    };

    web.nginx.virtualHost = mkOption {
      type = types.nullOr types.str;
      default = null;
      example = "zeroclaw.example.com";
      description = ''
        Hostname of the nginx virtualHost to proxy to the zeroclaw daemon.

        This module only adds a `location /` proxy block to the named
        virtualHost. You must declare the virtualHost separately to control
        SSL, ACME, listen addresses, and access rules. E.g. to enable TLS:

        services.nginx.virtualHosts."zeroclaw.example.com" = {
          forceSSL = true;
          enableACME = true;
        };
      '';
    };
  };

  config = mkIf cfg.enable {
    systemd.tmpfiles.rules = [
      "d ${cfg.stateDir}           0700 ${cfg.user} ${cfg.group} -"
      "d ${cfg.stateDir}/workspace 0700 ${cfg.user} ${cfg.group} -"
    ];

    users.users.${cfg.user} = {
      isSystemUser = true;
      group = cfg.group;
      home = cfg.stateDir;
      # Created by tmpfile rule above
      createHome = false;
      description = "ZeroClaw agent daemon user";
    };

    users.groups.${cfg.group} = { };

    systemd.services.zeroclaw = {
      description = "ZeroClaw Agent Daemon";
      wantedBy = [ "multi-user.target" ];
      after = [ "network-online.target" ];
      wants = [ "network-online.target" ];
      restartTriggers = [ configFile ];

      environment = {
        ZEROCLAW_CONFIG_DIR = cfg.stateDir;
        ZEROCLAW_GATEWAY_HOST = cfg.host;
        ZEROCLAW_GATEWAY_PORT = toString cfg.port;
      }
      # allow_public_bind is required when binding to a non-loopback address
      // optionalAttrs (cfg.host != "127.0.0.1" && cfg.host != "localhost" && cfg.host != "::1") {
        ZEROCLAW_ALLOW_PUBLIC_BIND = "1";
      };

      serviceConfig = {
        Type = "exec";
        User = cfg.user;
        Group = cfg.group;

        ExecStartPre = "+${mergeScript}";
        ExecStart = "${lib.getExe cfg.package} daemon";

        WorkingDirectory = cfg.stateDir;

        EnvironmentFile = lib.mkIf (cfg.environmentFile != null) cfg.environmentFile;

        # Hardening
        NoNewPrivileges = true;
        PrivateTmp = true;
        ProtectSystem = "strict";
        ProtectHome = true;
        ReadWritePaths = [ cfg.stateDir ];
        CapabilityBoundingSet = "";
        LockPersonality = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";

        Restart = "on-failure";
        RestartSec = "5s";
      };
    };

    services.nginx.virtualHosts = mkIf (cfg.web.nginx.virtualHost != null) {
      ${cfg.web.nginx.virtualHost} = {
        locations."/" = {
          proxyPass = "http://${cfg.host}:${toString cfg.port}";
          proxyWebsockets = true;
          extraConfig = ''
            proxy_set_header Host $host;
            proxy_set_header X-Real-IP $remote_addr;
          '';
        };
      };
    };
  };
}
