import SwiftUI

extension Color {
    /// ZeroClaw brand orange (#E85C0D)
    static let zeroClawOrange = Color(red: 232/255, green: 92/255, blue: 13/255)

    /// Dark background for dark mode (#1A1A2E)
    static let zeroClawDarkBg = Color(red: 26/255, green: 26/255, blue: 46/255)
}

extension ShapeStyle where Self == Color {
    static var zeroClawOrange: Color { .zeroClawOrange }
}
