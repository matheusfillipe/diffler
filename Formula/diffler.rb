class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.6"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.6/diffler-v0.2.6-aarch64-apple-darwin.tar.gz"
      sha256 "7bc8370acf2bec03be97f07af970e9da537b26c1973e4700629f88c9bf019e68"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.6/diffler-v0.2.6-x86_64-apple-darwin.tar.gz"
      sha256 "240a4f8478b92e221b2e8e07c667ba19637d12b2547f3a4659f91436e5ddcc26"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.6/diffler-v0.2.6-aarch64-unknown-linux-musl.tar.gz"
      sha256 "338dee86556a3111a5329b63e5d50e7ae0b1820ee0acc55ee35c7084d51c2243"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.6/diffler-v0.2.6-x86_64-unknown-linux-musl.tar.gz"
      sha256 "eee709c86ad61972a002985256c97d4f4ccc047e7d69d2fd0294efaea5446b51"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
