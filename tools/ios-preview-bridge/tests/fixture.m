#import <UIKit/UIKit.h>

@interface PreviewProofView : UIView
@property NSUInteger taps;
@property NSUInteger moves;
@property NSUInteger ends;
@property NSUInteger cancels;
@property CGPoint last;
@end
@implementation PreviewProofView
- (void)persist {
    NSDictionary *proof = @{@"taps":@(_taps), @"moves":@(_moves), @"ends":@(_ends), @"cancels":@(_cancels), @"x":@(_last.x), @"y":@(_last.y), @"width":@(self.bounds.size.width), @"height":@(self.bounds.size.height)};
    NSData *data = [NSJSONSerialization dataWithJSONObject:proof options:0 error:nil];
    NSString *directory = NSSearchPathForDirectoriesInDomains(NSDocumentDirectory, NSUserDomainMask, YES).firstObject;
    [data writeToFile:[directory stringByAppendingPathComponent:@"proof.json"] atomically:YES];
    [self setNeedsDisplay];
}
- (instancetype)initWithFrame:(CGRect)frame {
    if ((self = [super initWithFrame:frame])) { self.backgroundColor = UIColor.whiteColor; self.multipleTouchEnabled = NO; [self persist]; }
    return self;
}
- (void)touchesBegan:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event { (void)event; _taps++; _last = [touches.anyObject locationInView:self]; [self persist]; }
- (void)touchesMoved:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event { (void)event; _moves++; _last = [touches.anyObject locationInView:self]; [self persist]; }
- (void)touchesEnded:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event { (void)event; _ends++; _last = [touches.anyObject locationInView:self]; [self persist]; }
- (void)touchesCancelled:(NSSet<UITouch *> *)touches withEvent:(UIEvent *)event { (void)touches; (void)event; _cancels++; [self persist]; }
- (void)drawRect:(CGRect)rect {
    (void)rect;
    NSString *label = [NSString stringWithFormat:@"RiWork native stream\nTouches: %lu\nDrag moves: %lu\nx %.1f, y %.1f", (unsigned long)_taps, (unsigned long)_moves, _last.x, _last.y];
    [label drawInRect:CGRectMake(25, 150, self.bounds.size.width - 50, 250) withAttributes:@{NSFontAttributeName:[UIFont boldSystemFontOfSize:28], NSForegroundColorAttributeName:UIColor.blackColor}];
    [[UIColor colorWithHue:(_taps % 10) / 10.0 saturation:.7 brightness:.9 alpha:1] setFill];
    [[UIBezierPath bezierPathWithOvalInRect:CGRectMake(_last.x - 25, _last.y - 25, 50, 50)] fill];
}
@end
@interface PreviewProofApp : UIResponder <UIApplicationDelegate>
@property (nonatomic, strong) UIWindow *window;
@end
@implementation PreviewProofApp
- (void)recordBackground:(BOOL)background {
    NSString *directory = NSSearchPathForDirectoriesInDomains(NSDocumentDirectory, NSUserDomainMask, YES).firstObject;
    NSData *data = [NSJSONSerialization dataWithJSONObject:@{@"background":@(background)} options:0 error:nil];
    [data writeToFile:[directory stringByAppendingPathComponent:@"activity.json"] atomically:YES];
}
- (void)applicationDidEnterBackground:(UIApplication *)application { (void)application; [self recordBackground:YES]; }
- (void)applicationDidBecomeActive:(UIApplication *)application { (void)application; [self recordBackground:NO]; }
- (BOOL)application:(UIApplication *)application didFinishLaunchingWithOptions:(NSDictionary *)options {
    (void)application; (void)options;
    self.window = [[UIWindow alloc] initWithFrame:UIScreen.mainScreen.bounds];
    UIViewController *controller = [UIViewController new]; controller.view = [[PreviewProofView alloc] initWithFrame:self.window.bounds];
    self.window.rootViewController = controller; [self.window makeKeyAndVisible]; return YES;
}
@end
int main(int argc, char **argv) { @autoreleasepool { return UIApplicationMain(argc, argv, nil, NSStringFromClass(PreviewProofApp.class)); } }
