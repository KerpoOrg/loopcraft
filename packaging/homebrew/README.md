# Homebrew tap for LoopCraft

The published tap is the repository `KerpoOrg/homebrew-loopcraft`, which holds
`Formula/loopcraft.rb`. This folder keeps the source of that formula next to
the code it builds.

Users install with:

```sh
brew install KerpoOrg/loopcraft/loopcraft
loopcraft setup
```

`brew install KerpoOrg/loopcraft/loopcraft` taps `github.com/KerpoOrg/homebrew-loopcraft`
(Homebrew adds the `homebrew-` prefix) and builds LoopCraft from source.

## Releasing a version

1. Bump `version` in the workspace `Cargo.toml`, commit, tag `vX.Y.Z` and push the tag.
2. Get the tarball's checksum:
   `curl -sL https://github.com/KerpoOrg/loopcraft/archive/refs/tags/vX.Y.Z.tar.gz | shasum -a 256`
3. In the tap's `Formula/loopcraft.rb`, set `url` to that tag's tarball and `sha256`
   to the checksum, and commit.
4. Check it: `brew update && brew upgrade loopcraft && brew test loopcraft && loopcraft setup`.

## Testing a formula change locally

```sh
brew tap-new --no-git kerpoorg/loopcraft-test
cp packaging/homebrew/loopcraft.rb "$(brew --repository)/Library/Taps/kerpoorg/homebrew-loopcraft-test/Formula/"
# point url at a local tarball (file:///…) and set its sha256, then:
brew install --build-from-source kerpoorg/loopcraft-test/loopcraft
brew test kerpoorg/loopcraft-test/loopcraft
brew uninstall loopcraft && brew untap kerpoorg/loopcraft-test
```
