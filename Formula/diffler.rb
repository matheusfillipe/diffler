class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.2/diffler-v0.2.2-aarch64-apple-darwin.tar.gz"
      sha256 "50c48af61ccd1f7c4292065a9a271daceacec3068d0ae2bb7224d2d760b8bc1b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.2/diffler-v0.2.2-x86_64-apple-darwin.tar.gz"
      sha256 "c963e7361bad5dac6d6a05b29621fc22545900fe4e1b97870f87c925f1e105e6"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.2/diffler-v0.2.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "2984eedfc35afba59535dea86c4924b7e148975e9d36b8602d0ea98112d7a170"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.2/diffler-v0.2.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "ceaf01198af424ef6e12b87f015fbf05cd0faf0b596ccfa56bce35dad1756e85"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
