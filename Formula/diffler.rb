class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.8.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.8.0/diffler-v0.8.0-aarch64-apple-darwin.tar.gz"
      sha256 "f31a5d492979a01afa0a401da8c74a958a0f12273ccc8908f0a6a6a377ba3a34"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.8.0/diffler-v0.8.0-x86_64-apple-darwin.tar.gz"
      sha256 "dcd5b4bd153f16a24ae16cab8d4b562349dda5a54d7fd59f9fc1eaf25057b38a"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.8.0/diffler-v0.8.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "bc4df88eade0372a0baf47dd46fcfe66835e00c354474cb1e6e0c3368a607a7e"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.8.0/diffler-v0.8.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "57677c4693631ee67dd57e529702e936cd7cd7e4f8407d90556aec7d7d3b65e7"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
