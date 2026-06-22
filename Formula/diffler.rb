class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.0/diffler-v0.2.0-aarch64-apple-darwin.tar.gz"
      sha256 "69c32579c04c360251bb66ab3465689990b990331bba31e2433b076aba191254"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.0/diffler-v0.2.0-x86_64-apple-darwin.tar.gz"
      sha256 "b022366ba180b055773a2200cc0ef10589c016a954e70ab45cd6822010429afd"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.0/diffler-v0.2.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "28e7e8947206f59126d2d9a0db8d7a3a6782e4d206b25c61a67023ae0db75e21"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.0/diffler-v0.2.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "9f8946e1443d2cc7640a412262a1071348d5dceec4e912c60671d6432db53959"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
