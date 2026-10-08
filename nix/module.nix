# SPDX-License-Identifier: EUPL-1.2

self:
{
  config,
  lib,
  pkgs,
  ...
}:
let
  inherit (lib)
    getExe
    literalExpression
    mkEnableOption
    mkIf
    mkOption
    mkPackageOption
    types
    ;

  cfg = config.services.caesar;
in
{
  options.services.caesar = {
    enable = mkEnableOption "caesar, a small CalDAV and CardDAV server";

    package = mkPackageOption self.packages.${pkgs.stdenv.hostPlatform.system} "caesar" {
      default = "default";
      pkgsText = "self.packages.\${pkgs.stdenv.hostPlatform.system}";
    };

    socket = mkOption {
      type = types.path;
      default = "/run/caesar.sock";
      description = ''
        Unix socket caesar listens on. Point the reverse proxy at it and have
        it forward the authenticated username in the `Remote-User` header.
      '';
    };

    socketGroup = mkOption {
      type = types.str;
      example = literalExpression "config.services.caddy.group";
      description = ''
        Group allowed to connect to the socket, i.e. the reverse proxy's
        group. Only the proxy should be in it: whoever can connect can claim
        to be any user.
      '';
    };

    logLevel = mkOption {
      type = types.str;
      default = "info";
      example = "caesar=debug";
      description = "`RUST_LOG` filter.";
    };
  };

  config = mkIf cfg.enable {
    systemd.sockets.caesar = {
      description = "caesar CalDAV/CardDAV socket";
      wantedBy = [ "sockets.target" ];
      listenStreams = [ cfg.socket ];
      socketConfig = {
        SocketMode = "0660";
        SocketGroup = cfg.socketGroup;
      };
    };

    systemd.services.caesar = {
      description = "caesar CalDAV/CardDAV server";
      requires = [ "caesar.socket" ];
      after = [ "caesar.socket" ];
      environment.RUST_LOG = cfg.logLevel;

      serviceConfig = {
        ExecStart = getExe cfg.package;
        Restart = "on-failure";

        # Data lives in /var/lib/caesar (/var/lib/private/caesar on disk).
        DynamicUser = true;
        StateDirectory = "caesar";
        StateDirectoryMode = "0700";
        UMask = "0077";

        # The socket is inherited from caesar.socket; no network access needed.
        PrivateNetwork = true;
        RestrictAddressFamilies = [ "AF_UNIX" ];

        CapabilityBoundingSet = "";
        NoNewPrivileges = true;
        PrivateDevices = true;
        PrivateUsers = true;
        ProtectClock = true;
        ProtectControlGroups = true;
        ProtectHome = true;
        ProtectHostname = true;
        ProtectKernelLogs = true;
        ProtectKernelModules = true;
        ProtectKernelTunables = true;
        ProtectProc = "invisible";
        ProcSubset = "pid";
        LockPersonality = true;
        MemoryDenyWriteExecute = true;
        RestrictNamespaces = true;
        RestrictRealtime = true;
        RestrictSUIDSGID = true;
        SystemCallArchitectures = "native";
        SystemCallFilter = [
          "@system-service"
          "~@privileged"
          "~@resources"
        ];
      };
    };
  };
}
