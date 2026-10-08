# caesar

A small CalDAV + CardDAV server for personal use. Built for Thunderbird and
DAVx⁵.

## Setup

caesar has no authentication of its own; it sits behind a reverse proxy that
terminates TLS, checks a Basic auth password on every request and **always
overwrites** the `Remote-User` header with the authenticated username. caesar
listens on a Unix socket that only the proxy's group can connect to, so nothing
else on the machine can claim to be a user.

Add the flake input and module:

```nix
{
  inputs.caesar.url = "github:faukah/caesar";

  outputs = { nixpkgs, caesar, ... }: {
    nixosConfigurations.tower = nixpkgs.lib.nixosSystem {
      modules = [
        caesar.nixosModules.default
        ./dav.nix # one of the proxy configs below
      ];
    };
  };
}
```

This gives a socket-activated service on `/run/caesar.sock` with data in
`/var/lib/caesar`. Point DNS for your subdomain at the machine and open ports 80
and 443.

### Caddy

```nix
{ config, ... }:
{
  services.caesar = {
    enable = true;
    socketGroup = config.services.caddy.group;
  };

  services.caddy = {
    enable = true;
    virtualHosts."dav.example.com".extraConfig = ''
      basic_auth {
        alice <hash>
      }
      reverse_proxy unix//run/caesar.sock {
        header_up Remote-User {http.auth.user.id}
      }
    '';
  };
}
```

Generate `<hash>` with `nix run nixpkgs#caddy -- hash-password`. Add one line
per user. Caddy fetches the TLS certificate itself.

### nginx

```nix
{ config, ... }:
{
  services.caesar = {
    enable = true;
    socketGroup = config.services.nginx.group;
  };

  services.nginx = {
    enable = true;
    recommendedProxySettings = true;
    virtualHosts."dav.example.com" = {
      enableACME = true;
      forceSSL = true;
      locations."/" = {
        basicAuthFile = "/run/secrets/caesar-htpasswd";
        proxyPass = "http://unix:/run/caesar.sock";
        extraConfig = ''
          proxy_set_header Remote-User $remote_user;
          client_max_body_size 10m;
        '';
      };
    };
  };

  security.acme = {
    acceptTerms = true;
    defaults.email = "you@example.com";
  };
}
```

Create the password file with
`nix shell nixpkgs#apacheHttpd -c htpasswd -cB caesar-htpasswd alice` (drop `-c`
to add users) and put it at the path above, readable by nginx. Keep it out of
the Nix store, e.g. with sops-nix or agenix.

### Other proxies

Anything works that requires Basic auth on every request, sets `Remote-User`
from the authenticated user (replacing any value the client sent) and can proxy
to a Unix socket. Login flows that redirect to a web page don't work: CalDAV and
CardDAV clients only do Basic auth.

### Check and connect

```sh
curl -u alice -X PROPFIND -H 'Depth: 0' https://dav.example.com/alice/
```

should answer `207` with a `calendar-home-set`. A user's directory, default
calendar and address book are created on their first request. Logs:
`journalctl -u caesar`.

- **DAVx⁵**: tap "+", choose "Login with URL and user name" and enter
  `https://dav.example.com`.
- **Thunderbird calendars**: create a new calendar "On the Network" with the
  location `https://dav.example.com`.
- **Thunderbird contacts**: add a CardDAV address book with the same URL.

## Data

```
/var/lib/caesar/<user>/calendars/<name>/<item>.ics
/var/lib/caesar/<user>/contacts/<name>/<item>.vcf
```

Each collection has a `.props.toml` (name, colour, description) and a
`.sync.log`. Back up the directory with anything that copies files. Only edit it
while caesar is stopped; changes are picked up on the next start.

Deleted collections are moved to `<user>/.trash/`.

## Running locally

```sh
CAESAR_DATA_DIR=./data CAESAR_SOCKET=./caesar.sock cargo run
curl --unix-socket caesar.sock -X PROPFIND -H 'Remote-User: alice' \
  -H 'Depth: 1' http://localhost/alice/
```

caesar only listens on a Unix socket: either the one systemd passes it or
`$CAESAR_SOCKET` (default `./caesar.sock`). `RUST_LOG` sets the log filter
(default `info`).

## License

[EUPL-1.2](./LICENSE)
