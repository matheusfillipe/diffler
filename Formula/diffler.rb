class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.14.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.1/diffler-v0.14.1-aarch64-apple-darwin.tar.gz"
      sha256 "7f5e21437dc29e3a5ab3248bcbf4032688e596ac25a903916c26b1ea20e7d906"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.1/diffler-v0.14.1-x86_64-apple-darwin.tar.gz"
      sha256 "b66e04728cc3f52da92040b00a0a47b0b5982b17d9bcd70cab26d6eeb89d884c"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.1/diffler-v0.14.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "52c057552ce6c2b504cef3139578ba487375c53e51d85327655f0afa1441849c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.14.1/diffler-v0.14.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "4d363be38e6794f65d4cd72e75b7b4bc3ecb5cef7c227d6b6bf8c562adb87917"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
