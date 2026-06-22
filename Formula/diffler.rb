class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.3"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.3/diffler-v0.2.3-aarch64-apple-darwin.tar.gz"
      sha256 "a69eb1082e1c6e4aad6c22e2d681afef818e01b10b3e56903c9cce53355d2feb"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.3/diffler-v0.2.3-x86_64-apple-darwin.tar.gz"
      sha256 "75350b993020a88168ac945eddfa666e7535b1408e3eea9cf8b444ceae13ec9f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.3/diffler-v0.2.3-aarch64-unknown-linux-musl.tar.gz"
      sha256 "defffe115a696b5babfa9de66241aa87f3a1e2d13cc16ef554064f17d9894f75"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.3/diffler-v0.2.3-x86_64-unknown-linux-musl.tar.gz"
      sha256 "511e574dc23200d3295012ce9035e27733d496dfcbb9a6b77770762e8e782af1"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
