class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.4"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.4/diffler-v0.2.4-aarch64-apple-darwin.tar.gz"
      sha256 "13a1af747d0d0bdc39525fa111f125c4e8a861ddda154af86f9cad5e918671e0"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.4/diffler-v0.2.4-x86_64-apple-darwin.tar.gz"
      sha256 "0570afd9fffa9138a05625afc3d0585f7ca45a6f254f575433c1c8e96b8752be"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.4/diffler-v0.2.4-aarch64-unknown-linux-musl.tar.gz"
      sha256 "ccdb119d78cf471fc37a3f00e9ec685269745d67d863ab23f0688353005dbda7"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.4/diffler-v0.2.4-x86_64-unknown-linux-musl.tar.gz"
      sha256 "9c2d210d18fc28a1099adcf281c7c37d0a4cca1581e41773c0a5635adacf4973"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
