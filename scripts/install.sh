#!/bin/sh
# Installs gse from GitHub releases.
#
#   curl -fsSL https://github.com/danthegoodman1/git-secure-env/releases/latest/download/install.sh | sh
#
# GSE_VERSION picks a release tag (default: the latest). GSE_INSTALL_DIR picks
# the directory (default: ~/.local/bin). When that directory is missing from
# PATH, the installer adds it in your shell's startup files;
# GSE_NO_MODIFY_PATH=1 prints the line to add instead.
set -eu

repo="danthegoodman1/git-secure-env"

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64 | Linux-amd64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  Darwin-arm64) target=aarch64-apple-darwin ;;
  *)
    echo "gse: no prebuilt binary for $(uname -s) $(uname -m); build it with:" >&2
    echo "  cargo install --git https://github.com/$repo" >&2
    exit 1
    ;;
esac

version="${GSE_VERSION:-}"
if [ -z "$version" ]; then
  version="$(curl -fsSL "https://api.github.com/repos/$repo/releases/latest" |
    sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
fi
if [ -z "$version" ]; then
  echo "gse: cannot find the latest release" >&2
  exit 1
fi

name="gse-$version-$target"
url="https://github.com/$repo/releases/download/$version/$name.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl -fsSL "$url" -o "$tmp/$name.tar.gz"
curl -fsSL "$url.sha256" -o "$tmp/$name.tar.gz.sha256"
(
  cd "$tmp"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum -c "$name.tar.gz.sha256" >/dev/null
  else
    shasum -a 256 -c "$name.tar.gz.sha256" >/dev/null
  fi
)
tar -xzf "$tmp/$name.tar.gz" -C "$tmp"

dir="${GSE_INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$dir"
cp "$tmp/$name/gse" "$dir/gse"
chmod 755 "$dir/gse"
echo "gse: installed $version to $dir/gse"

# Appends a line to a startup file unless the file already has it.
add_line() {
  mkdir -p "$(dirname "$1")"
  if ! grep -qsF "$2" "$1"; then
    printf '\n# Added by the gse installer\n%s\n' "$2" >>"$1"
  fi
}

case ":$PATH:" in
  *":$dir:"*) ;;
  *)
    case "$dir" in
      "$HOME"/*) shown="\$HOME/${dir#"$HOME"/}" ;;
      *) shown="$dir" ;;
    esac
    line="case \":\$PATH:\" in *\":$shown:\"*) ;; *) export PATH=\"$shown:\$PATH\" ;; esac"
    shell="$(basename "${SHELL:-sh}")"
    if [ -n "${GSE_NO_MODIFY_PATH:-}" ]; then
      echo "gse: $dir is not on PATH; add it in your shell's startup file:"
      echo "  $line"
    else
      case "$shell" in
        zsh) set -- "${ZDOTDIR:-$HOME}/.zshenv" ;;
        bash)
          login="$HOME/.profile"
          for f in "$HOME/.bash_profile" "$HOME/.bash_login" "$HOME/.profile"; do
            if [ -f "$f" ]; then
              login="$f"
              break
            fi
          done
          set -- "$HOME/.bashrc" "$login"
          ;;
        fish) set -- "${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/gse.fish" ;;
        *) set -- "$HOME/.profile" ;;
      esac
      for f in "$@"; do
        if [ "$shell" = fish ]; then
          add_line "$f" "fish_add_path -g \"$shown\""
        else
          add_line "$f" "$line"
        fi
        echo "gse: added $dir to PATH in $f"
      done
      echo "gse: open a new terminal, or run: export PATH=\"$dir:\$PATH\""
    fi
    ;;
esac
echo "gse: next, run \`gse install\` and \`gse keygen\`"
