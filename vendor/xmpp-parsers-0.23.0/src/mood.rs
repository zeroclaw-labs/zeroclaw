// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Enum representing all of the possible values of the XEP-0107 moods.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::MOOD, exhaustive)]
pub enum MoodEnum {
    /// Impressed with fear or apprehension; in fear; apprehensive.
    #[xml(name = "afraid")]
    Afraid,

    /// Astonished; confounded with fear, surprise or wonder.
    #[xml(name = "amazed")]
    Amazed,

    /// Inclined to love; having a propensity to love, or to sexual enjoyment; loving, fond, affectionate, passionate, lustful, sexual, etc.
    #[xml(name = "amorous")]
    Amorous,

    /// Displaying or feeling anger, i.e., a strong feeling of displeasure, hostility or antagonism towards someone or something, usually combined with an urge to harm.
    #[xml(name = "angry")]
    Angry,

    /// To be disturbed or irritated, especially by continued or repeated acts.
    #[xml(name = "annoyed")]
    Annoyed,

    /// Full of anxiety or disquietude; greatly concerned or solicitous, esp. respecting something future or unknown; being in painful suspense.
    #[xml(name = "anxious")]
    Anxious,

    /// To be stimulated in one's feelings, especially to be sexually stimulated.
    #[xml(name = "aroused")]
    Aroused,

    /// Feeling shame or guilt.
    #[xml(name = "ashamed")]
    Ashamed,

    /// Suffering from boredom; uninterested, without attention.
    #[xml(name = "bored")]
    Bored,

    /// Strong in the face of fear; courageous.
    #[xml(name = "brave")]
    Brave,

    /// Peaceful, quiet.
    #[xml(name = "calm")]
    Calm,

    /// Taking care or caution; tentative.
    #[xml(name = "cautious")]
    Cautious,

    /// Feeling the sensation of coldness, especially to the point of discomfort.
    #[xml(name = "cold")]
    Cold,

    /// Feeling very sure of or positive about something, especially about one's own capabilities.
    #[xml(name = "confident")]
    Confident,

    /// Chaotic, jumbled or muddled.
    #[xml(name = "confused")]
    Confused,

    /// Feeling introspective or thoughtful.
    #[xml(name = "contemplative")]
    Contemplative,

    /// Pleased at the satisfaction of a want or desire; satisfied.
    #[xml(name = "contented")]
    Contented,

    /// Grouchy, irritable; easily upset.
    #[xml(name = "cranky")]
    Cranky,

    /// Feeling out of control; feeling overly excited or enthusiastic.
    #[xml(name = "crazy")]
    Crazy,

    /// Feeling original, expressive, or imaginative.
    #[xml(name = "creative")]
    Creative,

    /// Inquisitive; tending to ask questions, investigate, or explore.
    #[xml(name = "curious")]
    Curious,

    /// Feeling sad and dispirited.
    #[xml(name = "dejected")]
    Dejected,

    /// Severely despondent and unhappy.
    #[xml(name = "depressed")]
    Depressed,

    /// Defeated of expectation or hope; let down.
    #[xml(name = "disappointed")]
    Disappointed,

    /// Filled with disgust; irritated and out of patience.
    #[xml(name = "disgusted")]
    Disgusted,

    /// Feeling a sudden or complete loss of courage in the face of trouble or danger.
    #[xml(name = "dismayed")]
    Dismayed,

    /// Having one's attention diverted; preoccupied.
    #[xml(name = "distracted")]
    Distracted,

    /// Having a feeling of shameful discomfort.
    #[xml(name = "embarrassed")]
    Embarrassed,

    /// Feeling pain by the excellence or good fortune of another.
    #[xml(name = "envious")]
    Envious,

    /// Having great enthusiasm.
    #[xml(name = "excited")]
    Excited,

    /// In the mood for flirting.
    #[xml(name = "flirtatious")]
    Flirtatious,

    /// Suffering from frustration; dissatisfied, agitated, or discontented because one is unable to perform an action or fulfill a desire.
    #[xml(name = "frustrated")]
    Frustrated,

    /// Feeling appreciation or thanks.
    #[xml(name = "grateful")]
    Grateful,

    /// Feeling very sad about something, especially something lost; mournful; sorrowful.
    #[xml(name = "grieving")]
    Grieving,

    /// Unhappy and irritable.
    #[xml(name = "grumpy")]
    Grumpy,

    /// Feeling responsible for wrongdoing; feeling blameworthy.
    #[xml(name = "guilty")]
    Guilty,

    /// Experiencing the effect of favourable fortune; having the feeling arising from the consciousness of well-being or of enjoyment; enjoying good of any kind, as peace, tranquillity, comfort; contented; joyous.
    #[xml(name = "happy")]
    Happy,

    /// Having a positive feeling, belief, or expectation that something wished for can or will happen.
    #[xml(name = "hopeful")]
    Hopeful,

    /// Feeling the sensation of heat, especially to the point of discomfort.
    #[xml(name = "hot")]
    Hot,

    /// Having or showing a modest or low estimate of one's own importance; feeling lowered in dignity or importance.
    #[xml(name = "humbled")]
    Humbled,

    /// Feeling deprived of dignity or self-respect.
    #[xml(name = "humiliated")]
    Humiliated,

    /// Having a physical need for food.
    #[xml(name = "hungry")]
    Hungry,

    /// Wounded, injured, or pained, whether physically or emotionally.
    #[xml(name = "hurt")]
    Hurt,

    /// Favourably affected by something or someone.
    #[xml(name = "impressed")]
    Impressed,

    /// Feeling amazement at something or someone; or feeling a combination of fear and reverence.
    #[xml(name = "in_awe")]
    InAwe,

    /// Feeling strong affection, care, liking, or attraction..
    #[xml(name = "in_love")]
    InLove,

    /// Showing anger or indignation, especially at something unjust or wrong.
    #[xml(name = "indignant")]
    Indignant,

    /// Showing great attention to something or someone; having or showing interest.
    #[xml(name = "interested")]
    Interested,

    /// Under the influence of alcohol; drunk.
    #[xml(name = "intoxicated")]
    Intoxicated,

    /// Feeling as if one cannot be defeated, overcome or denied.
    #[xml(name = "invincible")]
    Invincible,

    /// Fearful of being replaced in position or affection.
    #[xml(name = "jealous")]
    Jealous,

    /// Feeling isolated, empty, or abandoned.
    #[xml(name = "lonely")]
    Lonely,

    /// Unable to find one's way, either physically or emotionally.
    #[xml(name = "lost")]
    Lost,

    /// Feeling as if one will be favored by luck.
    #[xml(name = "lucky")]
    Lucky,

    /// Causing or intending to cause intentional harm; bearing ill will towards another; cruel; malicious.
    #[xml(name = "mean")]
    Mean,

    /// Given to sudden or frequent changes of mind or feeling; temperamental.
    #[xml(name = "moody")]
    Moody,

    /// Easily agitated or alarmed; apprehensive or anxious.
    #[xml(name = "nervous")]
    Nervous,

    /// Not having a strong mood or emotional state.
    #[xml(name = "neutral")]
    Neutral,

    /// Feeling emotionally hurt, displeased, or insulted.
    #[xml(name = "offended")]
    Offended,

    /// Feeling resentful anger caused by an extremely violent or vicious attack, or by an offensive, immoral, or indecent act.
    #[xml(name = "outraged")]
    Outraged,

    /// Interested in play; fun, recreational, unserious, lighthearted; joking, silly.
    #[xml(name = "playful")]
    Playful,

    /// Feeling a sense of one's own worth or accomplishment.
    #[xml(name = "proud")]
    Proud,

    /// Having an easy-going mood; not stressed; calm.
    #[xml(name = "relaxed")]
    Relaxed,

    /// Feeling uplifted because of the removal of stress or discomfort.
    #[xml(name = "relieved")]
    Relieved,

    /// Feeling regret or sadness for doing something wrong.
    #[xml(name = "remorseful")]
    Remorseful,

    /// Without rest; unable to be still or quiet; uneasy; continually moving.
    #[xml(name = "restless")]
    Restless,

    /// Feeling sorrow; sorrowful, mournful.
    #[xml(name = "sad")]
    Sad,

    /// Mocking and ironical.
    #[xml(name = "sarcastic")]
    Sarcastic,

    /// Pleased at the fulfillment of a need or desire.
    #[xml(name = "satisfied")]
    Satisfied,

    /// Without humor or expression of happiness; grave in manner or disposition; earnest; thoughtful; solemn.
    #[xml(name = "serious")]
    Serious,

    /// Surprised, startled, confused, or taken aback.
    #[xml(name = "shocked")]
    Shocked,

    /// Feeling easily frightened or scared; timid; reserved or coy.
    #[xml(name = "shy")]
    Shy,

    /// Feeling in poor health; ill.
    #[xml(name = "sick")]
    Sick,

    /// Feeling the need for sleep.
    #[xml(name = "sleepy")]
    Sleepy,

    /// Acting without planning; natural; impulsive.
    #[xml(name = "spontaneous")]
    Spontaneous,

    /// Suffering emotional pressure.
    #[xml(name = "stressed")]
    Stressed,

    /// Capable of producing great physical force; or, emotionally forceful, able, determined, unyielding.
    #[xml(name = "strong")]
    Strong,

    /// Experiencing a feeling caused by something unexpected.
    #[xml(name = "surprised")]
    Surprised,

    /// Showing appreciation or gratitude.
    #[xml(name = "thankful")]
    Thankful,

    /// Feeling the need to drink.
    #[xml(name = "thirsty")]
    Thirsty,

    /// In need of rest or sleep.
    #[xml(name = "tired")]
    Tired,

    /// [Feeling any emotion not defined here.]
    #[xml(name = "undefined")]
    Undefined,

    /// Lacking in force or ability, either physical or emotional.
    #[xml(name = "weak")]
    Weak,

    /// Thinking about unpleasant things that have happened or that might happen; feeling afraid and unhappy.
    #[xml(name = "worried")]
    Worried,
}

generate_elem_id!(
    /// Free-form text description of the mood.
    Text,
    "text",
    MOOD
);

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(MoodEnum, 1);
        assert_size!(Text, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(MoodEnum, 1);
        assert_size!(Text, 24);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<happy xmlns='http://jabber.org/protocol/mood'/>"
            .parse()
            .unwrap();
        let mood = MoodEnum::try_from(elem).unwrap();
        assert_eq!(mood, MoodEnum::Happy);
    }

    #[test]
    fn test_text() {
        let elem: Element = "<text xmlns='http://jabber.org/protocol/mood'>Yay!</text>"
            .parse()
            .unwrap();
        let elem2 = elem.clone();
        let text = Text::try_from(elem).unwrap();
        assert_eq!(text.0, String::from("Yay!"));

        let elem3 = text.into();
        assert_eq!(elem2, elem3);
    }
}
