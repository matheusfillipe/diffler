class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.23.2"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.2/diffler-v0.23.2-aarch64-apple-darwin.tar.gz"
      sha256 "3e462ab68bc3f971dd345eccf868dcc838e58ca126231a0c2cd1954d050529c9"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.2/diffler-v0.23.2-x86_64-apple-darwin.tar.gz"
      sha256 "bf2088c2731ed919dda97e0d54673abc594ac312752ddd311627c0c087dbdf31"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.2/diffler-v0.23.2-aarch64-unknown-linux-musl.tar.gz"
      sha256 "e891f617109c601ff9d06394ff84ab91f1cad87935a53cf97acd8d5360c591fa"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.23.2/diffler-v0.23.2-x86_64-unknown-linux-musl.tar.gz"
      sha256 "1d0e33cd43a0c76797b1ab204c3626562ed9cf3e2f9981ecb6973691881895b7"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
