class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.5.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.1/diffler-v0.5.1-aarch64-apple-darwin.tar.gz"
      sha256 "626ca764b0a541d1873c9f33c5a1e086958569b089881c54a10b4483a9bb82fa"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.1/diffler-v0.5.1-x86_64-apple-darwin.tar.gz"
      sha256 "d116acc1683c81782295fb896c3685cf94325430838422b5efa184227388c4fa"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.1/diffler-v0.5.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "4c866ffa9b376db88b07e7fe20e771cbcc47bedec74488d621dc923f76ffc5dd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.5.1/diffler-v0.5.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d24ad5673b13f19f1dcc847f715a25b1c226ef1137557ce9105d35479307f254"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
