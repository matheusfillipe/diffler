class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.5"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.5/diffler-v0.2.5-aarch64-apple-darwin.tar.gz"
      sha256 "a9eb62a22dfdca380f35c2f08a70f3a9d47fe1e7fec94268f1a57a5119e5e2bc"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.5/diffler-v0.2.5-x86_64-apple-darwin.tar.gz"
      sha256 "91370eea7a1047a56000540d482cae0ece8cc612876c24833eadbdc1940dc8ae"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.5/diffler-v0.2.5-aarch64-unknown-linux-musl.tar.gz"
      sha256 "025cfeedee334948e9699a1660656c7c85b107b125eafbf028094c62cdb22301"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.5/diffler-v0.2.5-x86_64-unknown-linux-musl.tar.gz"
      sha256 "a46ce984e891a8f62d745b6dd56cd8eab13011c7cfcafd033bb1c9e0643e9236"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
