class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.0/diffler-v0.6.0-aarch64-apple-darwin.tar.gz"
      sha256 "4da1aff264ebf04b014cb5168f7355b3f07c785e845e7fde73600fc1e2323052"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.0/diffler-v0.6.0-x86_64-apple-darwin.tar.gz"
      sha256 "e9b185489527a56b3190117f5cf915cb3f290511772432049fa6f1268273f1e5"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.0/diffler-v0.6.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0a0b405213f1d4270fb630b3a648acd6deb15c636c1281438cfd13524a1c2362"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.0/diffler-v0.6.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "883b1b3f58346cd1a1bfe7aaf5113bf079c35e5351429b19797c204e48967a4f"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
