class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.7.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.7.0/diffler-v0.7.0-aarch64-apple-darwin.tar.gz"
      sha256 "9b25a3671ff9294129e590ca8be1faa1181f627e832401fb04945b3c96a0569f"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.7.0/diffler-v0.7.0-x86_64-apple-darwin.tar.gz"
      sha256 "46c33b41872912c7b5d6640c91024094c60a510bf0464e060966fc0847c41b46"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.7.0/diffler-v0.7.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "53a7ba97201da3a1513652c07776a8080599881dfb46ddc5506f6cd6cb0a3c78"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.7.0/diffler-v0.7.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "8fc3d8156a3c393b57084e029f2cee42d04123b7f37e4a298284130fe78a44bf"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
