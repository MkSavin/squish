class Squish < Formula
  desc "Batch video re-encoder on top of ffmpeg"
  homepage "https://github.com/MkSavin/squish"
  version "0.1.0"
  license "MIT"

  depends_on "ffmpeg"

  on_macos do
    on_arm do
      url "https://github.com/MkSavin/squish/releases/download/v0.1.0/squish-v0.1.0-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/MkSavin/squish/releases/download/v0.1.0/squish-v0.1.0-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
    end
  end

  def install
    bin.install "squish"
  end

  test do
    assert_match "squish", shell_output("#{bin}/squish --version")
  end
end
