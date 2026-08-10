class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.10.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.0/diffler-v0.10.0-aarch64-apple-darwin.tar.gz"
      sha256 "31fc1fe8df5d2f8adbdb15eae2c4efbfcff67baeea1688171dfaba8b5c012b44"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.0/diffler-v0.10.0-x86_64-apple-darwin.tar.gz"
      sha256 "b315ba90aee224cf97e8a7d122042e935744399894391006a2f657a8c17c6465"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.0/diffler-v0.10.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "0a9b0d9698ea4b3ac87ca2ab326d370a681f8c63ac477ba7ad1d94db3f5662be"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.10.0/diffler-v0.10.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "208bd6d0890c6396ef396f126b3935500de107d5e0cfa50731018922237a13d8"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
