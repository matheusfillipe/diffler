class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.8"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.8/diffler-v0.6.8-aarch64-apple-darwin.tar.gz"
      sha256 "e721905f940c5ff4b938f9c400134e8216bfcd9597b3d70d8821366cb7692126"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.8/diffler-v0.6.8-x86_64-apple-darwin.tar.gz"
      sha256 "449d476082ca7f53671554f36de61116a56a918da233afa0bbd9e08b8814410b"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.8/diffler-v0.6.8-aarch64-unknown-linux-musl.tar.gz"
      sha256 "61bb0001241b0aaf77132336a880871ce8ec5225bcdbb5bd9aa91b43302bb645"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.8/diffler-v0.6.8-x86_64-unknown-linux-musl.tar.gz"
      sha256 "1cd8844cf393481330889dccc04696dfbf074d1882c5f1f1a8f2b6e2e0bb1483"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
