# Homebrew cask for TernaryGrid.
# Publish the notarized DMG produced by scripts/bundle-macos.sh to the release page,
# then update `version` and `sha256` (printed by the script). Install it with:
#   brew install --cask ./packaging/homebrew/ternarygrid.rb
cask "ternarygrid" do
  version "0.6.2"
  sha256 "e8b642b8ff8b7c626f72b82bceb274bf1645b7989cdff7717f248d3e4daa9914"

  url "https://github.com/ilhamnadhif/ternarygrid/releases/download/v#{version}/TernaryGrid-#{version}.dmg"
  name "TernaryGrid"
  desc "Lightweight SQL database client written in Rust"
  homepage "https://github.com/ilhamnadhif/ternarygrid"

  depends_on macos: ">= :monterey"

  app "TernaryGrid.app"
  binary "#{appdir}/TernaryGrid.app/Contents/MacOS/ternarygrid"

  zap trash: [
    "~/Library/Application Support/dev.ternarygrid.TernaryGrid",
    "~/Library/Caches/dev.ternarygrid.TernaryGrid",
    "~/Library/Preferences/dev.ternarygrid.TernaryGrid.plist",
  ]
end
