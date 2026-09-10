// Host Swift/UniFFI wire smoke. Compile alongside freshly generated bindings;
// see docs/gift-card-metadata.md. This is not an iOS app or a UI test.
import Foundation

@main
struct GiftCardSmoke {
    static let today = DateYmd(year: 2026, month: 9, day: 9)
    static let options = ParseOptions(ruleDocuments: [], knownMerchants: [])

    static func parse(_ text: String) throws -> ReceiptResult {
        let detections = text.split(separator: "\n").enumerated().map { index, line in
            let y = 80.0 + Double(index) * 35.0
            return DetectionInput(pointsXy: [80, y, 600, y, 600, y + 20, 80, y + 20],
                                  text: String(line), confidence: 0.99)
        }
        return try parseDetections(detections: detections, paddedWidth: 800, paddedHeight: 1600,
                                   padding: 50, imageFilename: "synthetic.jpg", today: today,
                                   creditCardAccount: "Liabilities:Card", currency: "CAD",
                                   taxAccount: "Expenses:Tax", imageSha256: nil, options: options)
    }

    static func reformat(_ previous: ReceiptResult, tenders: [ReceiptTender]? = nil,
                         items: [EditedItem]? = nil) throws -> ReceiptResult {
        try reformatReceipt(previous: previous, today: today, creditCardAccount: "Liabilities:Card",
                            currency: "CAD", taxAccount: "Expenses:Tax", imageSha256: nil,
                            edits: ReceiptEdits(tenders: tenders, merchant: nil, dateIso: nil,
                                                items: items, total: nil, tax: nil, subtotal: nil),
                            options: options)
    }

    static func main() throws {
        let receipt = try parse("LCBO\nBOTTLE 59.70\nTOTAL 59.70\nGift Card 50.00\n123456xxxxx9876543x EXP:NONE\nAUTHOR.#:123456 BAL:0.00\nGift Card 9.70\n123456xxxxx1112223x EXP:NONE\nAUTHOR.#:789012 BAL:90.30")
        precondition(receipt.tenders.count == 2)
        let original = receipt.tenders[1].giftCard!
        precondition(receipt.tenders[0].giftCard?.remainingBalanceCents == 0)
        precondition(original.remainingBalanceCents == 9030 && original.expiry == .noExpiry)
        precondition(original.currency == "CAD" && original.normalizedIdentifier == "123456*****1112223*")
        var tenders = receipt.tenders
        tenders[1].giftCard!.remainingBalanceCents = 9000
        let corrected = try reformat(receipt, tenders: tenders)
        let card = corrected.tenders[1].giftCard!
        precondition(card.remainingBalanceCents == 9000)
        precondition(card.evidence == original.evidence)
        precondition(card.correctedFields == ["remaining_balance_cents"])
        let reloaded = try reformat(corrected)
        precondition(reloaded.tenders[1].giftCard == card)

        let purchase = try parse("COSTCO\n399 DOORDASH2X50 79.99\nPC 111111 ACTIVATED\n399 DOORDASH2X50 79.99\nPC 222222 ACTIVATED\nSUBTOTAL 159.98\nTOTAL 159.98\nMASTERCARD 159.98")
        precondition(purchase.items.count == 2)
        precondition(purchase.items[0].giftCard?.activation == .activated)
        precondition(purchase.items[0].giftCard?.totalFaceValueCents == 10000)
        let reordered = purchase.items.reversed().map { item in
            EditedItem(giftCard: item.giftCard, description: item.description,
                       itemNumber: item.itemNumber, price: item.price, quantity: item.quantity, tagPath: "")
        }
        let moved = try reformat(purchase, items: reordered)
        precondition(moved.items[0].giftCard == purchase.items[1].giftCard)
        precondition(moved.items[1].giftCard == purchase.items[0].giftCard)
        print("PASS: Swift/UniFFI extraction, optional zero, enums, corrections, evidence, and reordered purchases")
    }
}
