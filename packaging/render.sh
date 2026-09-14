#!/usr/bin/env bash
# Rewrites every packaging manifest for one released version, taking the
# checksums from the .sha256 files attached to that release.
#
#   ./packaging/render.sh 0.2.0
#
# Without a version it re-reads the one already in the manifests, which is how
# you fill in checksums after a release finishes uploading.
set -euo pipefail

repo="MkSavin/squish"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
placeholder="0000000000000000000000000000000000000000000000000000000000000000"

version="${1:-$(sed -n 's/^version = "\(.*\)"/\1/p' "$here/../Cargo.toml" | head -1)}"
version="${version#v}"
tag="v$version"
base="https://github.com/$repo/releases/download/$tag"

windows="squish-$tag-x86_64-pc-windows-msvc.zip"
linux="squish-$tag-x86_64-unknown-linux-gnu.tar.gz"
macos="squish-$tag-aarch64-apple-darwin.tar.gz"

# A release that is still building has no checksum yet; the manifest is written
# anyway, with a placeholder that is obviously not a hash.
checksum() {
  curl -fsSL "$base/$1.sha256" 2>/dev/null | tr -d '[:space:]' || echo "$placeholder"
}

windows_sha="$(checksum "$windows")"
# winget wants the checksum in capitals; macOS still ships a bash without ${x^^}.
windows_sha_upper="$(printf '%s' "$windows_sha" | tr '[:lower:]' '[:upper:]')"
linux_sha="$(checksum "$linux")"
macos_sha="$(checksum "$macos")"

mkdir -p "$here/scoop" "$here/homebrew" "$here/winget"

cat > "$here/scoop/squish.json" <<JSON
{
    "version": "$version",
    "description": "Batch video re-encoder on top of ffmpeg, hardware constant quality by default",
    "homepage": "https://github.com/$repo",
    "license": "MIT",
    "depends": "ffmpeg",
    "architecture": {
        "64bit": {
            "url": "$base/$windows",
            "hash": "$windows_sha",
            "extract_dir": "${windows%.zip}"
        }
    },
    "bin": "squish.exe",
    "checkver": "github",
    "autoupdate": {
        "architecture": {
            "64bit": {
                "url": "https://github.com/$repo/releases/download/v\$version/squish-v\$version-x86_64-pc-windows-msvc.zip",
                "extract_dir": "squish-v\$version-x86_64-pc-windows-msvc"
            }
        },
        "hash": {
            "url": "\$url.sha256"
        }
    }
}
JSON

cat > "$here/homebrew/squish.rb" <<RUBY
class Squish < Formula
  desc "Batch video re-encoder on top of ffmpeg"
  homepage "https://github.com/$repo"
  version "$version"
  license "MIT"

  depends_on "ffmpeg"

  on_macos do
    on_arm do
      url "$base/$macos"
      sha256 "$macos_sha"
    end
  end

  on_linux do
    on_intel do
      url "$base/$linux"
      sha256 "$linux_sha"
    end
  end

  def install
    bin.install "squish"
  end

  test do
    assert_match "squish", shell_output("#{bin}/squish --version")
  end
end
RUBY

cat > "$here/winget/MkSavin.Squish.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.version.1.6.0.schema.json
PackageIdentifier: MkSavin.Squish
PackageVersion: $version
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.6.0
YAML

cat > "$here/winget/MkSavin.Squish.installer.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.installer.1.6.0.schema.json
PackageIdentifier: MkSavin.Squish
PackageVersion: $version
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: ${windows%.zip}\\squish.exe
    PortableCommandAlias: squish
Installers:
  - Architecture: x64
    InstallerUrl: $base/$windows
    InstallerSha256: $windows_sha_upper
ReleaseDate: $(date -u +%Y-%m-%d)
ManifestType: installer
ManifestVersion: 1.6.0
YAML

cat > "$here/winget/MkSavin.Squish.locale.en-US.yaml" <<YAML
# yaml-language-server: \$schema=https://aka.ms/winget-manifest.defaultLocale.1.6.0.schema.json
PackageIdentifier: MkSavin.Squish
PackageVersion: $version
PackageLocale: en-US
Publisher: MkSavin
PublisherUrl: https://github.com/MkSavin
PackageName: squish
PackageUrl: https://github.com/$repo
License: MIT
LicenseUrl: https://github.com/$repo/blob/main/LICENSE
ShortDescription: Batch video re-encoder on top of ffmpeg
Description: >-
  squish re-encodes a whole folder of video with one command. It expands its own
  globs, reads each file with ffprobe to pick a profile and a pixel format the
  encoder accepts, encodes several files at once on the GPU, checks the results
  and never touches the originals.
Moniker: squish
Tags:
  - ffmpeg
  - nvenc
  - video
  - transcode
  - cli
ReleaseNotesUrl: https://github.com/$repo/releases/tag/$tag
ManifestType: defaultLocale
ManifestVersion: 1.6.0
YAML

printf 'rendered %s\n' "$tag"
for name in "$windows:$windows_sha" "$linux:$linux_sha" "$macos:$macos_sha"; do
  sha="${name##*:}"
  [ "$sha" = "$placeholder" ] && printf '  no checksum yet for %s\n' "${name%%:*}"
done
exit 0
