# Homebrew packaging

On each `v*` tag, `.github/workflows/release.yml`:

1. Builds native binaries for macOS (x86_64 + arm64), produces a universal
   macOS binary, and builds Linux binaries (x86_64 + arm64).
2. Packages each into `llmproxy-<tag>-<target>.tar.gz` and attaches them to
   the GitHub Release.
3. Renders `packaging/brew/llmproxy.rb.template` with the release URLs and
   SHA256 sums, then commits the result to `Formula/llmproxy.rb` in your
   Homebrew tap repository.

## One-time setup

### 1. Create the tap repository

The convention is `<owner>/homebrew-tap` (brew strips the `homebrew-`
prefix in `brew tap <owner>/tap`). Create it on GitHub as a regular public
repo, empty is fine:

```sh
# On github.com, create TomeHirata/homebrew-tap (or use the gh CLI locally).
```

The publish workflow will create `Formula/llmproxy.rb` inside it on the
first tag push.

### 2. Add a `HOMEBREW_TAP_TOKEN` secret

The job that commits into the tap repo needs a token with `contents: write`
on that repo. Two options:

- **Fine-grained PAT** (recommended): scope `Contents: Read and write` on
  `<owner>/homebrew-tap` only.
- **Classic PAT**: `repo` scope (broader than needed).

Add it under the source repo's **Settings -> Secrets and variables ->
Actions** as `HOMEBREW_TAP_TOKEN`.

### 3. (Optional) Populate extra formula fields

The template intentionally omits `license` because the upstream package has
none set yet. Once you add a `LICENSE` file and set `license = "<SPDX>"` in
`crates/llmproxy-server/Cargo.toml`, add a matching `license "<SPDX>"` line
to `packaging/brew/llmproxy.rb.template`.

## End-user install

```sh
brew tap tomehirata/tap
brew install llmproxy
```

Replace `tomehirata` with the lowercased owner of the repo hosting the tap.

## Local iteration

Render the formula locally to check syntax before cutting a tag:

```sh
export TAG=v0.0.0 VERSION=0.0.0 \
       REPO_URL=https://github.com/TomeHirata/llm-proxy \
       DOWNLOAD_BASE=https://example.invalid \
       SHA_DARWIN_ARM64=$(printf 'x%.0s' {1..64}) \
       SHA_DARWIN_X86_64=$(printf 'x%.0s' {1..64}) \
       SHA_LINUX_ARM64=$(printf 'x%.0s' {1..64}) \
       SHA_LINUX_X86_64=$(printf 'x%.0s' {1..64})
envsubst < packaging/brew/llmproxy.rb.template
```

Or audit a committed formula:

```sh
brew audit --strict --online <owner>/tap/llmproxy
```

## Troubleshooting

- **`Permission denied` when pushing to tap**: `HOMEBREW_TAP_TOKEN` is
  missing, expired, or lacks write access to `<owner>/homebrew-tap`.
- **`sha256 mismatch` on `brew install`**: the binary attached to the
  GitHub Release differs from the SHA in the formula. This usually means
  the release assets were re-uploaded after the formula was committed.
  Re-run the `publish-brew` job, or delete and recreate the release and
  re-tag.
- **`brew audit` warns about missing `license`**: add a `license` line to
  the formula template once upstream license is finalized.
