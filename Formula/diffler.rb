class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.5.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.0/diffler-v0.5.0-aarch64-apple-darwin.tar.gz"
      sha256 "fc2c61f8610a60ccb89e673ce8555d7693c218700b42d39d0d2280b8ef7332e9"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.0/diffler-v0.5.0-x86_64-apple-darwin.tar.gz"
      sha256 "d21b2eea79fb3122f391b18ab52a7d61ed56bade770274d8befaf1695560d12f"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.0/diffler-v0.5.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "b9c1df393eecfe109310eae41c922e45a245562f1b15373d28410d65b50a0b34"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.0/diffler-v0.5.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "9ab559d29bcd7ef488394022cb7e87b3f8d04b6a10362d0629b82e8174af1f41"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
