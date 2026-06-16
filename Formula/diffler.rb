class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.12"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.12/diffler-v0.1.12-aarch64-apple-darwin.tar.gz"
      sha256 "da562d7f592f3a75f4de5ec6733acf2cc9a814d27e4d6b17714b2dbd9845bc18"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.12/diffler-v0.1.12-x86_64-apple-darwin.tar.gz"
      sha256 "bf2c68e82765f45fa7a475c0262d98d82aa7be6c294c7d29be867813582402b2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.12/diffler-v0.1.12-aarch64-unknown-linux-musl.tar.gz"
      sha256 "226d32721ba3bd38d69e8d587a108af9107f3e87fc0811d54d4c9ef9bc7ddd74"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.12/diffler-v0.1.12-x86_64-unknown-linux-musl.tar.gz"
      sha256 "eb5ab4ee0c89e2c3f01a567a5ccfb06241611041b501e4cbedc177f25580b649"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
