class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.21.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.1/diffler-v0.21.1-aarch64-apple-darwin.tar.gz"
      sha256 "3388e532ecabcb00ae8ed5351070468ace336492e940b6e335b3a154b62daa64"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.1/diffler-v0.21.1-x86_64-apple-darwin.tar.gz"
      sha256 "417109b4d0f41590e22c5f6c04565a8937a240274c7bfc2cb8bf6933fa07bba2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.1/diffler-v0.21.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "2d101d1e0bc1e08413983645589127819e503bf03eb6716c5c4fb0be6f4d3041"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.21.1/diffler-v0.21.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "7229057f20703ae2929c77698e3b400f623130f14f2987abd2fec6cb91a976e5"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
