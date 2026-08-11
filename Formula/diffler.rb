class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.10.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.1/diffler-v0.10.1-aarch64-apple-darwin.tar.gz"
      sha256 "0e77cbb25d5b71a1b29c23941d574bae842511a4701406ae2375361ed3f3e473"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.1/diffler-v0.10.1-x86_64-apple-darwin.tar.gz"
      sha256 "71b5aeada97ec504a346181e8775e7c8a195ddb2ed0d1f3600e50c7a04729167"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.1/diffler-v0.10.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "94f3805d3b41346ad6660bd7a12646f54205ceaea6630bb85fdbf2b72d98a3be"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.1/diffler-v0.10.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d23b4b64a7ee97cb28eaf188e726321b91fbe7a24816465b267e32e527076ce8"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
