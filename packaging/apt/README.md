# APT packaging

This directory configures the Debian/Ubuntu release pipeline. The
`Release` workflow (`.github/workflows/release.yml`) builds `.deb` packages
per-distro, per-arch, signs them, and publishes an APT repository to the
`gh-pages` branch, served via GitHub Pages.

## Supported targets

| Distro | Codename | Architectures |
| ------ | -------- | ------------- |
| Debian 12 | `bookworm` | amd64, arm64 |
| Debian 13 | `trixie`   | amd64, arm64 |
| Ubuntu 22.04 | `jammy` | amd64, arm64 |
| Ubuntu 24.04 | `noble` | amd64, arm64 |

Each `.deb` is built inside a container of the target distro (with QEMU for
arm64), so shared-library dependencies are resolved against the right libc
and the package's `Depends` are correct for that release.

## One-time setup

Before the first `v*` tag will successfully publish, these steps must be
completed on the GitHub repo.

### 1. Add a `LICENSE` file at the repo root

`cargo-deb` is configured with `license-file = ["../../LICENSE", "0"]`.
Without a license file the build will fail. Pick a license (MIT, Apache-2.0,
etc.), commit `LICENSE`, and optionally add a matching `license = "..."`
field to `crates/llmproxy-server/Cargo.toml`.

### 2. Generate a GPG signing key

Do this on a trusted machine, **not** on CI:

```sh
gpg --quick-gen-key "llmproxy apt signing <you@example.com>" rsa4096 sign 2y
KEY_ID=$(gpg --list-secret-keys --with-colons | awk -F: '/^sec:/ {print $5; exit}')
gpg --armor --export-secret-keys "$KEY_ID" > apt-signing.key
gpg --armor --export "$KEY_ID" > apt-signing.pub
```

Store `apt-signing.key` somewhere safe offline. You will paste it into a
GitHub Actions secret in the next step.

### 3. Add GitHub Actions secrets

In the repo's **Settings -> Secrets and variables -> Actions**, add:

- `APT_GPG_PRIVATE_KEY` - contents of `apt-signing.key` (ASCII-armored).
- `APT_GPG_PASSPHRASE` - passphrase for the key, if any. If the key is
  unprotected, set this to an empty string (still define the secret).

### 4. Initialize the `gh-pages` branch

The publish job checks out `gh-pages` and commits the apt tree into it.
Create it once, empty, so the first run has something to check out:

```sh
git switch --orphan gh-pages
git commit --allow-empty -m "Initialize gh-pages"
git push -u origin gh-pages
git switch -
```

Then in **Settings -> Pages**, set the source to branch `gh-pages`, path `/`.

### 5. (Optional) Pin `reprepro`'s distributions list

`distributions.template` is rendered at CI time with the key ID. If you want
to add or remove suites, edit the template.

## End-user install instructions

Replace `<owner>` with the GitHub owner (lowercased) that hosts this repo -
e.g. for `TomeHirata/llm-proxy`, use `tomehirata`.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://<owner>.github.io/llm-proxy/apt/pubkey.asc \
  | sudo gpg --dearmor -o /etc/apt/keyrings/llmproxy.gpg
echo "deb [signed-by=/etc/apt/keyrings/llmproxy.gpg] \
https://<owner>.github.io/llm-proxy/apt $(lsb_release -cs) main" \
  | sudo tee /etc/apt/sources.list.d/llmproxy.list
sudo apt-get update
sudo apt-get install llmproxy
```

The default config ships at `/etc/llmproxy/config.yaml` and is marked as a
conffile, so your edits survive upgrades.

## Local iteration

To build a `.deb` locally without the full CI matrix:

```sh
cargo install cargo-deb --locked
cargo deb -p llmproxy-server
ls target/debian/*.deb
```

## Troubleshooting

- **`cargo-deb` fails with "no license"**: add a `LICENSE` file at the repo
  root (see step 1).
- **`reprepro` errors with `gpg: signing failed: Inappropriate ioctl`**: the
  loopback pinentry config is missing - check the `Import GPG signing key`
  step in the workflow.
- **`smoke-test-apt` times out**: GitHub Pages can take a few minutes to
  publish a freshly pushed commit. The job retries for up to 5 minutes; if
  your Pages deploy is slower, bump the retry count.
- **Users see `NO_PUBKEY`**: the `pubkey.asc` on Pages doesn't match the key
  the `Release` file was signed with. Re-run the publish job after fixing
  `APT_GPG_PRIVATE_KEY`.
