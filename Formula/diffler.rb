class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.16.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.1/diffler-v0.16.1-aarch64-apple-darwin.tar.gz"
      sha256 "2ce1ad2360c39470cf7c70468a9af48539353f23ec8fb50e86b16b3c6b11b1ac"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.1/diffler-v0.16.1-x86_64-apple-darwin.tar.gz"
      sha256 "d650cf87ca928d795b1be3c7e79a08567ad8de0bd4a121d15ea82e9567322680"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.1/diffler-v0.16.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "d16279ac2e3cbc77443a34e49e3e2f8336ca210106db3157a4607dcde27c6c4b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.1/diffler-v0.16.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "4dea835221c33ed95c237bac7f9957cccdfbe76d1ab06a96c51daba0f3d0cff4"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
