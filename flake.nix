{
  description = "SAK - Swiss Army Knife for LLMs";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
  };

  outputs = { self, nixpkgs }:
    let
      supportedSystems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs supportedSystems;
    in
    {
      packages = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "sak";
            version =
              (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

            src = pkgs.lib.cleanSourceWith {
              src = ./.;
              filter = path: type:
                let
                  baseName = builtins.baseNameOf path;
                in
                baseName != "target"
                && baseName != "result"
                && baseName != ".beads"
                && baseName != ".git";
            };

            cargoLock.lockFile = ./Cargo.lock;

            nativeBuildInputs = with pkgs; [
              pkg-config
              cmake
            ];

            buildInputs = with pkgs; [
              openssl
            ] ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
              apple-sdk_15
            ];

            meta = with pkgs.lib; {
              description =
                "Swiss Army Knife for LLMs - read-only operations tool";
              license = licenses.mit;
              mainProgram = "sak";
            };
          };
        });

      devShells = forAllSystems (system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          default = pkgs.mkShell {
            inputsFrom = [ self.packages.${system}.default ];
            packages = with pkgs; [
              rustc
              cargo
              clippy
              rustfmt
              rust-analyzer
              # External CLIs the shell-out domains invoke at runtime (helm,
              # k8s, talos, gh). sak adds no Rust deps for these — they're
              # spawned via each domain's client.rs chokepoint — so the dev
              # shell provides them to make the dogfood / live-test surface
              # reproducible inside `nix develop`. NB: the Kubernetes package
              # manager is `kubernetes-helm` in nixpkgs (it installs a `helm`
              # binary); the bare `helm` attribute is an unrelated tool.
              kubernetes-helm
              kubectl
              talosctl
              gh
            ];
          };
        });
    };
}
