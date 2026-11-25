import XCTest
@testable import Nanoctl

final class VTDecoderTests: XCTestCase {
    func testAVCCStripsParameterSetsAndPreservesMultiSlicePayloadOrder() {
        let sps: [UInt8] = [0x67, 0x42, 0xc0, 0x1f, 0x00, 0x00, 0x03, 0x00]
        let pps: [UInt8] = [0x68, 0xce, 0x3c, 0x80]
        let aud: [UInt8] = [0x09, 0xf0]
        let firstSlice: [UInt8] = [0x65, 0xb8, 0x00, 0x04, 0x11]
        let sei: [UInt8] = [0x06, 0x05, 0x02, 0xaa, 0xbb]
        let secondSlice: [UInt8] = [0x65, 0x18, 0xe0, 0x00, 0x22]
        let au = annexB(sps, pps, aud, firstSlice, sei, secondSlice)

        var parameters: [Data] = []
        let converted = VTDecoder.avcc(au, codec: .h264, parameters: &parameters)

        XCTAssertEqual(parameters, [Data(sps), Data(pps)])
        XCTAssertEqual(converted, avcc(aud, firstSlice, sei, secondSlice))
    }

    func testAVCCRejectsParameterSetOnlyAccessUnits() {
        var parameters: [Data] = []
        let au = annexB([0x67, 0x42], [0x68, 0xce])
        XCTAssertNil(VTDecoder.avcc(au, codec: .h264, parameters: &parameters))
        XCTAssertEqual(parameters.count, 2)
    }

    private func annexB(_ nals: [UInt8]...) -> Data {
        var out = Data()
        for nal in nals {
            out.append(contentsOf: [0, 0, 0, 1])
            out.append(contentsOf: nal)
        }
        return out
    }

    private func avcc(_ nals: [UInt8]...) -> Data {
        var out = Data()
        for nal in nals {
            var length = UInt32(nal.count).bigEndian
            withUnsafeBytes(of: &length) { out.append(contentsOf: $0) }
            out.append(contentsOf: nal)
        }
        return out
    }
}
