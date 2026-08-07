class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.9.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.1/diffler-v0.9.1-aarch64-apple-darwin.tar.gz"
      sha256 "9197016d6d0d665c9f1be93e89557d0a25845236a0c83e1d21fd333d5500d83e"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.1/diffler-v0.9.1-x86_64-apple-darwin.tar.gz"
      sha256 "c454487c7e160d58a33e2b4c9856599b3db0e00e135b617c46d93fb131bdc1aa"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.1/diffler-v0.9.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "42e0666eeef9b6057c7891aee0e045577807368d3d0c9f1720ca3d49fdbdb8fc"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.1/diffler-v0.9.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "d7f6f7af3d5754dd46abea74640d8219c17e149362eec1f0d259dea8151f252f"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
