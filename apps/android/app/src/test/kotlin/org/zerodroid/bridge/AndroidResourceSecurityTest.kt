package org.zerodroid.bridge

import org.junit.Assert.assertEquals
import org.junit.Test
import org.w3c.dom.Element
import java.io.File
import javax.xml.parsers.DocumentBuilderFactory

class AndroidResourceSecurityTest {
    private val privateDomains = setOf("root", "file", "sharedpref", "database", "external")

    @Test
    fun preAndroid12BackupExcludesEveryPrivateStorageDomain() {
        assertEquals(privateDomains, excludedDomains("backup_rules.xml"))
    }

    @Test
    fun cloudBackupExcludesEveryPrivateStorageDomain() {
        assertEquals(
            privateDomains,
            excludedDomains("data_extraction_rules.xml", "cloud-backup"),
        )
    }

    @Test
    fun deviceTransferExcludesEveryPrivateStorageDomain() {
        assertEquals(
            privateDomains,
            excludedDomains("data_extraction_rules.xml", "device-transfer"),
        )
    }

    private fun excludedDomains(fileName: String, section: String? = null): Set<String> {
        val document = DocumentBuilderFactory.newInstance().newDocumentBuilder()
            .parse(mainSource("res/xml/$fileName"))
        val parent = if (section == null) {
            document.documentElement
        } else {
            document.getElementsByTagName(section).item(0) as? Element
                ?: throw AssertionError("missing <$section> in $fileName")
        }
        val exclusions = parent.getElementsByTagName("exclude")
        return buildSet {
            for (index in 0 until exclusions.length) {
                val exclusion = exclusions.item(index) as Element
                assertEquals(
                    "all data in each domain must be excluded",
                    ".",
                    exclusion.getAttribute("path"),
                )
                add(exclusion.getAttribute("domain"))
            }
        }
    }

    private fun mainSource(relativePath: String): File {
        val roots = listOf(
            File("src/main"),
            File("app/src/main"),
            File("apps/android/app/src/main"),
        )
        return roots.asSequence()
            .map { it.resolve(relativePath) }
            .firstOrNull(File::isFile)
            ?: throw AssertionError("cannot locate Android main source file: $relativePath")
    }
}
